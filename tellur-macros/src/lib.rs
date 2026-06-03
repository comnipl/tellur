//! Attribute macros that turn a function- or struct-style component definition
//! into a `VectorComponent` / `RasterComponent` plus a [`bon`] builder.
//!
//! ```ignore
//! #[component(vector)]
//! fn BouncingDot(#[available] available: Vec2, t: LocalTime) -> impl VectorComponent {
//!     // `available` is the parent-assigned size at render time.
//!     // `t` is a regular struct field / builder member.
//!     // The body returns a component tree.
//! }
//!
//! #[component(vector)]
//! pub struct Padding {
//!     pub insets: EdgeInsets,
//!     #[builder(into)]
//!     pub child: Box<dyn VectorComponent>,
//! }
//! ```
//!
//! ## Function form
//!
//! A function-form component is written in PascalCase (mirroring the type it
//! defines and React's component functions); the name is PascalCase-normalized
//! to derive the struct, so a `BouncingDot` fn expands to a `BouncingDot`
//! struct whose fields are the non-`#[available]` function arguments. Since the
//! fn is consumed by the macro and re-emitted as a struct, no `non_snake_case`
//! lint fires. The function body becomes a
//! private `__tellur_build` helper; the trait impl forwards `layout`,
//! `paint_bounds`, and `render` to the built body. An `#[available]` argument
//! is threaded through the layout protocol rather than stored as a field.
//!
//! ## Struct form
//!
//! Leaves the user's `impl VectorComponent` / `RasterComponent` untouched and
//! just attaches the builder machinery to the struct.
//!
//! ## Builder machinery (both forms)
//!
//! Every component gets `#[derive(bon::Builder)]` with `derive(Into)`, plus:
//! - `From<T>` and `From<TBuilder<IsComplete>>` for `Box<dyn _Component>`, so a
//!   built value or a *complete builder* flows into a parent's
//!   `child(impl Into<Box<dyn _>>)` setter with no explicit `.build()`; and
//! - an impl of `VectorBuilder` / `RasterBuilder` on the complete builder, which
//!   backs the blanket `place_at` / `anchored` / `rasterize` extensions.
//!
//! A field/argument annotated `#[children(each = name)]` must be a `Vec<_>`; it
//! becomes a `#[builder(field)]` and gains streaming setters: a singular
//! `name(impl Into<Item>)` (push) and a plural `<field-name>(IntoIterator)`
//! (extend), each with a `maybe_`-prefixed counterpart — `maybe_name(Option<_>)`
//! and `maybe_<field-name>(Option<IntoIterator>)` — that adds nothing when the
//! argument is `None`. All four add to the same vec, so they interleave and
//! preserve order. All other `#[builder(...)]` attributes pass straight through
//! to `bon`.
//!
//! A raster component's child field/argument annotated `#[effect]` (which must be
//! a `Box<dyn RasterComponent>`) additionally gets an `Effect` impl on its builder
//! *while the child slot is unset*, so callers can write
//! `base.effect(ThisEffect::builder()…)` instead of nesting `.child(base)`. It is
//! purely additive — the normal `.child(...)` setter and the component itself are
//! untouched — and is rejected on `#[component(vector)]`.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{
    parse_macro_input, parse_quote, Attribute, DeriveInput, Field, FnArg, Ident, Item, ItemFn,
    ItemStruct, Meta, Pat, PatType, Type,
};

mod keyable;

/// Derives `PartialEq`, `Eq`, and `Hash` for a type that contains `f32`/`f64`
/// (directly or through `Option`/`Vec`/arrays/tuples), comparing and hashing
/// every float by its bit pattern. Unlike the stock derives this yields a
/// mutually consistent, `Eq`-sound trio even though floats are neither `Eq`
/// nor `Hash`. Non-float fields delegate to their own `PartialEq`/`Hash`.
#[proc_macro_derive(Keyable)]
pub fn keyable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match keyable::derive(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Kind of component the macro targets.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Vector,
    Raster,
    Timeline,
}

impl Kind {
    fn component_trait(self) -> TokenStream2 {
        match self {
            Kind::Vector => quote!(::tellur_core::vector::VectorComponent),
            Kind::Raster => quote!(::tellur_core::raster::RasterComponent),
            Kind::Timeline => quote!(::tellur_core::timeline_component::TimelineComponent),
        }
    }

    fn builder_trait(self) -> TokenStream2 {
        match self {
            Kind::Vector => quote!(::tellur_core::builder::VectorBuilder),
            Kind::Raster => quote!(::tellur_core::builder::RasterBuilder),
            Kind::Timeline => quote!(::tellur_core::timeline_component::TimelineBuilder),
        }
    }

    /// The `Box<dyn _Component>` the glue converts into. The timeline arm adds
    /// `+ Send` (audit M2: a `TimelineComponent` must be boxable with `+ Send`);
    /// the raster/vector arms must NOT, or existing `!Send` components break.
    fn box_dyn(self) -> TokenStream2 {
        let comp = self.component_trait();
        match self {
            Kind::Vector | Kind::Raster => quote!(::std::boxed::Box<dyn #comp>),
            Kind::Timeline => quote!(::std::boxed::Box<dyn #comp + ::core::marker::Send>),
        }
    }

    fn graphic(self) -> TokenStream2 {
        match self {
            Kind::Vector => quote!(::tellur_core::vector::VectorGraphic),
            Kind::Raster => quote!(::tellur_core::raster::RasterImage),
            // The timeline arm has no single `render` graphic; it emits a full
            // multi-method trait impl directly (see `expand_fn`).
            Kind::Timeline => quote!(()),
        }
    }

    /// The `render` signature and the forwarded argument list.
    fn render_sig(self) -> (TokenStream2, TokenStream2) {
        match self {
            Kind::Vector => (quote!(size: ::tellur_core::geometry::Vec2), quote!(size)),
            Kind::Raster => (
                quote!(
                    size: ::tellur_core::geometry::Vec2,
                    target: ::tellur_core::raster::Resolution,
                    ctx: &mut dyn ::tellur_core::render_context::RenderContext
                ),
                quote!(size, target, ctx),
            ),
            // Unused for the timeline arm; it builds its own method set.
            Kind::Timeline => (quote!(), quote!()),
        }
    }
}

/// `#[component(vector)]` / `#[component(raster)]`.
#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    let kind = match syn::parse::<Ident>(attr) {
        Ok(id) if id == "vector" => Kind::Vector,
        Ok(id) if id == "raster" => Kind::Raster,
        Ok(id) if id == "timeline" => Kind::Timeline,
        _ => {
            return syn::Error::new(
                Span::call_site(),
                "expected `#[component(vector)]`, `#[component(raster)]`, or `#[component(timeline)]`",
            )
            .to_compile_error()
            .into();
        }
    };
    expand_item(item, kind)
}

/// Backwards-compatible alias for `#[component(vector)]` on a function.
#[proc_macro_attribute]
pub fn vector_component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_item(item, Kind::Vector)
}

/// Backwards-compatible alias for `#[component(raster)]` on a function.
#[proc_macro_attribute]
pub fn raster_component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_item(item, Kind::Raster)
}

fn expand_item(item: TokenStream, kind: Kind) -> TokenStream {
    let item = parse_macro_input!(item as Item);
    let result = match item {
        Item::Fn(func) => expand_fn(func, kind),
        Item::Struct(s) => expand_struct(s, kind),
        other => Err(syn::Error::new_spanned(
            other,
            "#[component] can only be applied to a function or a struct",
        )),
    };
    match result {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// A `#[children(each = name)]` collection member.
struct Children {
    field: Ident,
    item_ty: Type,
    each: Option<Ident>,
}

// ─── struct form ───────────────────────────────────────────────────────────

fn expand_struct(mut s: ItemStruct, kind: Kind) -> syn::Result<TokenStream2> {
    if !s.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &s.generics,
            "#[component] struct cannot be generic",
        ));
    }
    let syn::Fields::Named(named) = &mut s.fields else {
        return Err(syn::Error::new_spanned(
            &s.fields,
            "#[component] struct must have named fields",
        ));
    };

    let mut children: Option<Children> = None;
    let mut effect_child: Option<Ident> = None;
    for field in named.named.iter_mut() {
        // `#[effect]`: tag this field as the effect-child slot (raster only).
        if field.attrs.iter().any(|a| a.path().is_ident("effect")) {
            effect_child = Some(parse_effect_field(field, kind, effect_child.is_some())?);
        }

        let Some(pos) = field
            .attrs
            .iter()
            .position(|a| a.path().is_ident("children"))
        else {
            continue;
        };
        if children.is_some() {
            return Err(syn::Error::new_spanned(
                &field.attrs[pos],
                "#[component] supports at most one #[children] field",
            ));
        }
        let attr = field.attrs.remove(pos);
        let each = parse_children_each(&attr)?;
        let item_ty = vec_inner(&field.ty).ok_or_else(|| {
            syn::Error::new_spanned(&field.ty, "#[children] field must be a `Vec<_>`")
        })?;
        field.attrs.push(parse_quote!(#[builder(field)]));
        children = Some(Children {
            field: field.ident.clone().unwrap(),
            item_ty,
            each,
        });
    }

    let ident = s.ident.clone();
    let glue = emit_glue(&ident, kind, &children, &effect_child);
    Ok(quote! {
        #[derive(::tellur_core::__bon::Builder)]
        #[builder(derive(Into), crate = ::tellur_core::__bon)]
        #s

        #glue
    })
}

// ─── function form ───────────────────────────────────────────────────────────

fn expand_fn(func: ItemFn, kind: Kind) -> syn::Result<TokenStream2> {
    if let Some(c) = func.sig.constness {
        return Err(syn::Error::new_spanned(c, "component fn cannot be const"));
    }
    if let Some(a) = func.sig.asyncness {
        return Err(syn::Error::new_spanned(a, "component fn cannot be async"));
    }
    if !func.sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &func.sig.generics,
            "component fn cannot have generic parameters (use concrete types as fields)",
        ));
    }

    let vis = &func.vis;
    let fn_ident = &func.sig.ident;
    let struct_ident = snake_to_pascal_ident(fn_ident);

    let mut field_idents: Vec<&Ident> = Vec::new();
    let mut field_types: Vec<&Type> = Vec::new();
    let mut field_attrs: Vec<Vec<Attribute>> = Vec::new();
    let mut available_ident: Option<&Ident> = None;
    let mut available_type: Option<&Type> = None;
    let mut clock_ident: Option<&Ident> = None;
    let mut clock_type: Option<&Type> = None;
    let mut children: Option<Children> = None;
    let mut effect_child: Option<Ident> = None;

    for arg in &func.sig.inputs {
        let FnArg::Typed(PatType { pat, ty, attrs, .. }) = arg else {
            return Err(syn::Error::new_spanned(
                arg,
                "component fn must not take a self receiver",
            ));
        };
        let Pat::Ident(pi) = pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                pat,
                "component fn argument must be a plain identifier",
            ));
        };

        if attrs.iter().any(|a| a.path().is_ident("available")) {
            if available_ident.is_some() {
                return Err(syn::Error::new_spanned(
                    pi,
                    "component fn can have at most one #[available] argument",
                ));
            }
            available_ident = Some(&pi.ident);
            available_type = Some(ty.as_ref());
            continue;
        }

        // `#[clock] clock: Clock`: the temporal twin of `#[available]`. Captured
        // and stripped here (so it is neither a struct field nor a cache-key
        // term) and threaded into `__tellur_build` by value; the generated
        // `frame`/`samples` forward the real framework clock. Only valid on the
        // timeline arm — there is no clock in the raster/vector render protocol.
        if attrs.iter().any(|a| a.path().is_ident("clock")) {
            if kind != Kind::Timeline {
                return Err(syn::Error::new_spanned(
                    pi,
                    "#[clock] is only valid on a #[component(timeline)]",
                ));
            }
            if clock_ident.is_some() {
                return Err(syn::Error::new_spanned(
                    pi,
                    "component fn can have at most one #[clock] argument",
                ));
            }
            clock_ident = Some(&pi.ident);
            clock_type = Some(ty.as_ref());
            continue;
        }

        // Forward `#[builder(...)]` attrs to the struct field; capture and
        // strip `#[children(...)]` into a streaming-collection field.
        let mut forwarded: Vec<Attribute> = Vec::new();
        for a in attrs {
            if a.path().is_ident("children") {
                if children.is_some() {
                    return Err(syn::Error::new_spanned(
                        a,
                        "component fn supports at most one #[children] argument",
                    ));
                }
                let each = parse_children_each(a)?;
                let item_ty = vec_inner(ty).ok_or_else(|| {
                    syn::Error::new_spanned(ty, "#[children] argument must be a `Vec<_>`")
                })?;
                forwarded.push(parse_quote!(#[builder(field)]));
                children = Some(Children {
                    field: pi.ident.clone(),
                    item_ty,
                    each,
                });
            } else if a.path().is_ident("effect") {
                // Drop the attr (don't forward it to bon) and record the slot.
                if !matches!(kind, Kind::Raster) {
                    return Err(syn::Error::new_spanned(
                        a,
                        "#[effect] is only valid on a #[component(raster)]",
                    ));
                }
                if effect_child.is_some() {
                    return Err(syn::Error::new_spanned(
                        a,
                        "component fn supports at most one #[effect] argument",
                    ));
                }
                if !is_boxed_raster_component(ty) {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "#[effect] argument must be of type `Box<dyn RasterComponent>`",
                    ));
                }
                effect_child = Some(pi.ident.clone());
            } else if a.path().is_ident("builder") {
                forwarded.push(a.clone());
            } else {
                return Err(syn::Error::new_spanned(
                    a,
                    "unsupported attribute on component fn argument (allowed: #[available], #[children(...)], #[builder(...)], #[effect])",
                ));
            }
        }

        field_idents.push(&pi.ident);
        field_types.push(ty.as_ref());
        field_attrs.push(forwarded);
    }

    let body = &func.block;
    let trait_path = kind.component_trait();
    let build_method = Ident::new("__tellur_build", fn_ident.span());

    // `#[available]` and `#[clock]` are mutually exclusive injection paths —
    // the former belongs to the raster/vector layout protocol, the latter to
    // the timeline frame protocol — but they never coexist on one fn (the
    // timeline arm has no `#[available]` and the others reject `#[clock]`).
    let (build_fn, trait_impl) = if kind == Kind::Timeline {
        timeline_codegen(&struct_ident, &trait_path, &build_method, field_idents.iter().copied(), clock_ident, clock_type, body)
    } else {
        let (render_sig, render_args) = kind.render_sig();
        let graphic_path = kind.graphic();
        let (build_fn, build_call_layout, build_call_render, build_call_paint_bounds) =
            if let (Some(av_ident), Some(av_type)) = (available_ident, available_type) {
                (
                    quote! {
                        #[doc(hidden)]
                        fn #build_method(&self, #av_ident: #av_type) -> impl #trait_path + 'static {
                            let Self { #( #field_idents ),* } = ::core::clone::Clone::clone(self);
                            #body
                        }
                    },
                    quote! {
                        let __available = ::tellur_core::geometry::Vec2(
                            if constraints.max.0.is_finite() { constraints.max.0 } else { 0.0 },
                            if constraints.max.1.is_finite() { constraints.max.1 } else { 0.0 },
                        );
                        let __child = self.#build_method(__available);
                        #trait_path::layout(&__child, constraints)
                    },
                    quote! {
                        let __child = self.#build_method(size);
                        #trait_path::render(&__child, #render_args)
                    },
                    quote! {
                        let __child = self.#build_method(size);
                        #trait_path::paint_bounds(&__child, size)
                    },
                )
            } else {
                (
                    quote! {
                        #[doc(hidden)]
                        fn #build_method(&self) -> impl #trait_path + 'static {
                            let Self { #( #field_idents ),* } = ::core::clone::Clone::clone(self);
                            #body
                        }
                    },
                    quote! {
                        let __child = self.#build_method();
                        #trait_path::layout(&__child, constraints)
                    },
                    quote! {
                        let __child = self.#build_method();
                        #trait_path::render(&__child, #render_args)
                    },
                    quote! {
                        let __child = self.#build_method();
                        #trait_path::paint_bounds(&__child, size)
                    },
                )
            };
        let trait_impl = quote! {
            impl #trait_path for #struct_ident {
                fn layout(
                    &self,
                    constraints: ::tellur_core::geometry::Constraints,
                ) -> ::tellur_core::geometry::Vec2 {
                    #build_call_layout
                }

                fn paint_bounds(
                    &self,
                    size: ::tellur_core::geometry::Vec2,
                ) -> ::tellur_core::geometry::Rect {
                    #build_call_paint_bounds
                }

                fn render(&self, #render_sig) -> #graphic_path {
                    #build_call_render
                }
            }
        };
        (build_fn, trait_impl)
    };

    // Float-aware `PartialEq`/`Eq`/`Hash` so the synthesized component is a
    // consistent cache key even with bare `f32`/`f64` fields (which are neither
    // `Eq` nor `Hash`). Shares its codegen with `#[derive(Keyable)]`, so
    // `Option<f32>`, `Vec<f32>`, etc. are handled too — not just bare floats.
    let eq_terms: Vec<keyable::EqTerm> = field_idents
        .iter()
        .zip(field_types.iter())
        .map(|(id, ty)| ((*ty).clone(), quote!(self.#id), quote!(other.#id)))
        .collect();
    let hash_terms: Vec<keyable::HashTerm> = field_idents
        .iter()
        .zip(field_types.iter())
        .map(|(id, ty)| ((*ty).clone(), quote!(self.#id)))
        .collect();
    let eq_body = keyable::eq_body_from(&eq_terms);
    let hash_body = keyable::hash_body_from(&hash_terms, &quote!(state));

    let glue = emit_glue(&struct_ident, kind, &children, &effect_child);

    Ok(quote! {
        #[derive(::core::clone::Clone, ::tellur_core::__bon::Builder)]
        #[builder(derive(Into), crate = ::tellur_core::__bon)]
        #vis struct #struct_ident {
            #( #( #field_attrs )* pub #field_idents: #field_types, )*
        }

        impl ::core::cmp::PartialEq for #struct_ident {
            fn eq(&self, other: &Self) -> bool {
                #eq_body
            }
        }

        impl ::core::cmp::Eq for #struct_ident {}

        impl ::core::hash::Hash for #struct_ident {
            fn hash<__H: ::core::hash::Hasher>(&self, state: &mut __H) {
                #hash_body
            }
        }

        impl #struct_ident {
            #build_fn
        }

        #trait_impl

        #glue
    })
}

/// Emits the `__tellur_build` helper and the full `TimelineComponent` impl for a
/// `#[component(timeline)]` fn (audit M3): every query
/// (`duration`/`measure`/`resolve`/`frame`/`samples`/`cues`/`arrangement`)
/// build-then-delegates to the body, mirroring how the raster arm delegates
/// `layout`/`render`. Without this, a `#[component(timeline)]` is an opaque leaf
/// and its inner children's starts never compose.
///
/// `frame`/`samples` forward the framework-supplied clock; the clock-less
/// queries build with [`Clock::structural`] — sound because a component's
/// resolved STRUCTURE must not depend on the clock value (only its baked
/// per-frame fields may).
fn timeline_codegen<'a>(
    struct_ident: &Ident,
    trait_path: &TokenStream2,
    build_method: &Ident,
    field_idents: impl Iterator<Item = &'a Ident> + Clone,
    clock_ident: Option<&Ident>,
    clock_type: Option<&Type>,
    body: &syn::Block,
) -> (TokenStream2, TokenStream2) {
    let destructure_idents: Vec<&Ident> = field_idents.collect();

    // The body is built either with the real clock (when `#[clock]` is present)
    // or with no clock at all. `__tellur_build` takes a clock by value in the
    // first case so every delegator forwards exactly the clock it has.
    let (build_fn, build_with_clock, build_structural) =
        if let (Some(ck_ident), Some(ck_type)) = (clock_ident, clock_type) {
            (
                quote! {
                    #[doc(hidden)]
                    fn #build_method(&self, #ck_ident: #ck_type) -> impl #trait_path + 'static {
                        let Self { #( #destructure_idents ),* } = ::core::clone::Clone::clone(self);
                        #body
                    }
                },
                quote!(self.#build_method(clock)),
                quote!(self.#build_method(::tellur_core::timeline_component::Clock::structural())),
            )
        } else {
            (
                quote! {
                    #[doc(hidden)]
                    fn #build_method(&self) -> impl #trait_path + 'static {
                        let Self { #( #destructure_idents ),* } = ::core::clone::Clone::clone(self);
                        #body
                    }
                },
                quote!(self.#build_method()),
                quote!(self.#build_method()),
            )
        };

    let trait_impl = quote! {
        impl #trait_path for #struct_ident {
            fn duration(&self) -> ::core::option::Option<f32> {
                let __child = #build_structural;
                #trait_path::duration(&__child)
            }

            fn measure(&self) -> ::core::option::Option<f32> {
                let __child = #build_structural;
                #trait_path::measure(&__child)
            }

            fn resolve(
                &self,
                abs_start: f32,
                out: &mut ::tellur_core::timeline_component::ResolveCtx,
            ) -> f32 {
                let __child = #build_structural;
                #trait_path::resolve(&__child, abs_start, out)
            }

            fn frame(
                &self,
                clock: ::tellur_core::timeline_component::Clock<'_>,
                target: ::tellur_core::raster::Resolution,
                ctx: &mut dyn ::tellur_core::render_context::RenderContext,
            ) -> ::core::option::Option<::tellur_core::raster::RasterImage> {
                let __child = #build_with_clock;
                #trait_path::frame(&__child, clock, target, ctx)
            }

            fn samples(
                &self,
                clock: ::tellur_core::timeline_component::Clock<'_>,
                window: f32,
            ) -> ::core::option::Option<::tellur_core::timeline_component::AudioBuffer> {
                let __child = #build_with_clock;
                #trait_path::samples(&__child, clock, window)
            }

            fn cues(&self, offset: f32) -> ::std::vec::Vec<::tellur_core::timeline_component::Cue> {
                let __child = #build_structural;
                #trait_path::cues(&__child, offset)
            }

            fn arrangement(&self) -> ::tellur_core::timeline_component::Arrangement {
                let __child = #build_structural;
                #trait_path::arrangement(&__child)
            }
        }
    };

    (build_fn, trait_impl)
}

// ─── shared builder glue ─────────────────────────────────────────────────────

/// Emits, for component type `ident`:
/// - `From<ident>` and `From<identBuilder<IsComplete>>` for `Box<dyn _>`,
/// - the `VectorBuilder` / `RasterBuilder` marker on the complete builder,
/// - streaming child setters when a `#[children]` member is present.
fn emit_glue(
    ident: &Ident,
    kind: Kind,
    children: &Option<Children>,
    effect_child: &Option<Ident>,
) -> TokenStream2 {
    let builder_ty = format_ident!("{}Builder", ident);
    let state_mod = Ident::new(&pascal_to_snake(&builder_ty.to_string()), ident.span());
    let bld = kind.builder_trait();
    // The timeline arm boxes as `Box<dyn TimelineComponent + Send>` (audit M2);
    // the raster/vector arms keep their plain `Box<dyn _Component>` (no `+ Send`,
    // which would break existing `!Send` components).
    let box_dyn = kind.box_dyn();

    // For a raster component whose child field is tagged `#[effect]`, implement
    // `Effect` on the builder *while that child slot is still unset*: `apply`
    // fills the child and finishes the build. The `SetChild<S>: IsComplete`
    // bound means "setting the child completes the builder" — i.e. every other
    // required member is already set — so a forgotten parameter is reported at
    // the `.effect(...)` call site. `Output` is the concrete component, keeping
    // `.effect().effect()` chaining and `.place_at()` working.
    let effect_impl = match (kind, effect_child) {
        (Kind::Raster, Some(field)) => {
            let field_pascal = snake_to_pascal_ident(field);
            let set_ty = format_ident!("Set{}", field_pascal);
            quote! {
                impl<__S> ::tellur_core::builder::Effect for #builder_ty<__S>
                where
                    __S: #state_mod::State,
                    __S::#field_pascal: #state_mod::IsUnset,
                    #state_mod::#set_ty<__S>: #state_mod::IsComplete,
                {
                    type Output = #ident;
                    fn apply(self, child: #box_dyn) -> #ident {
                        #builder_ty::build(self.#field(child))
                    }
                }
            }
        }
        _ => quote! {},
    };

    let children_methods = children.as_ref().map(|c| {
        let field = &c.field;
        let item_ty = &c.item_ty;
        let maybe_field = format_ident!("maybe_{}", field);
        let each_method = c.each.as_ref().map(|each| {
            let maybe_each = format_ident!("maybe_{}", each);
            quote! {
                pub fn #each(mut self, child: impl ::core::convert::Into<#item_ty>) -> Self {
                    self.#field.push(::core::convert::Into::into(child));
                    self
                }

                /// Pushes one child, or nothing when `child` is `None`.
                pub fn #maybe_each(
                    mut self,
                    child: ::core::option::Option<impl ::core::convert::Into<#item_ty>>,
                ) -> Self {
                    if let ::core::option::Option::Some(child) = child {
                        self.#field.push(::core::convert::Into::into(child));
                    }
                    self
                }
            }
        });
        quote! {
            impl<__S: #state_mod::State> #builder_ty<__S> {
                #each_method

                pub fn #field<__I, __T>(mut self, children: __I) -> Self
                where
                    __I: ::core::iter::IntoIterator<Item = __T>,
                    __T: ::core::convert::Into<#item_ty>,
                {
                    self.#field
                        .extend(children.into_iter().map(::core::convert::Into::into));
                    self
                }

                /// Extends with the iterator, or nothing when `children` is `None`.
                pub fn #maybe_field<__I, __T>(
                    mut self,
                    children: ::core::option::Option<__I>,
                ) -> Self
                where
                    __I: ::core::iter::IntoIterator<Item = __T>,
                    __T: ::core::convert::Into<#item_ty>,
                {
                    if let ::core::option::Option::Some(children) = children {
                        self.#field
                            .extend(children.into_iter().map(::core::convert::Into::into));
                    }
                    self
                }
            }
        }
    });

    quote! {
        impl ::core::convert::From<#ident> for #box_dyn {
            fn from(component: #ident) -> Self {
                ::std::boxed::Box::new(component)
            }
        }

        impl<__S: #state_mod::IsComplete> ::core::convert::From<#builder_ty<__S>> for #box_dyn {
            fn from(builder: #builder_ty<__S>) -> Self {
                ::std::boxed::Box::new(#builder_ty::build(builder))
            }
        }

        impl<__S: #state_mod::IsComplete> #bld for #builder_ty<__S> {
            type Output = #ident;
            fn build_component(self) -> #ident {
                #builder_ty::build(self)
            }
        }

        #children_methods

        #effect_impl
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Parses `#[children]` (→ `None`) or `#[children(each = name)]` (→ `Some`).
fn parse_children_each(attr: &Attribute) -> syn::Result<Option<Ident>> {
    match &attr.meta {
        Meta::Path(_) => Ok(None),
        Meta::List(_) => {
            let mut each = None;
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("each") {
                    each = Some(meta.value()?.parse()?);
                    Ok(())
                } else {
                    Err(meta.error("expected `each = name`"))
                }
            })?;
            Ok(each)
        }
        Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "expected `#[children]` or `#[children(each = name)]`",
        )),
    }
}

/// Strips `#[effect]` from a struct field, validates it, and returns the field
/// ident (the effect-child slot). Errors for non-raster components, a second
/// `#[effect]`, or a field that isn't a `Box<dyn RasterComponent>`.
fn parse_effect_field(field: &mut Field, kind: Kind, already: bool) -> syn::Result<Ident> {
    let pos = field
        .attrs
        .iter()
        .position(|a| a.path().is_ident("effect"))
        .expect("caller checked an #[effect] attr is present");
    let attr = field.attrs.remove(pos);
    if !matches!(kind, Kind::Raster) {
        return Err(syn::Error::new_spanned(
            &attr,
            "#[effect] is only valid on a #[component(raster)]",
        ));
    }
    if already {
        return Err(syn::Error::new_spanned(
            &attr,
            "#[component] supports at most one #[effect] field",
        ));
    }
    if !is_boxed_raster_component(&field.ty) {
        return Err(syn::Error::new_spanned(
            &field.ty,
            "#[effect] field must be of type `Box<dyn RasterComponent>`",
        ));
    }
    Ok(field.ident.clone().unwrap())
}

/// Whether `ty` is a `Box<dyn …RasterComponent>` (the only legal `#[effect]`
/// field type). Tolerant of a qualified path on the trait.
fn is_boxed_raster_component(ty: &Type) -> bool {
    let normalized: String = quote!(#ty).to_string().split_whitespace().collect();
    normalized.starts_with("Box<dyn") && normalized.ends_with("RasterComponent>")
}

/// Returns the `T` of a `Vec<T>` type, if `ty` is one.
fn vec_inner(ty: &Type) -> Option<Type> {
    let Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Vec" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(ab) = &seg.arguments else {
        return None;
    };
    match ab.args.first()? {
        syn::GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    }
}

fn snake_to_pascal_ident(ident: &Ident) -> Ident {
    let s = ident.to_string();
    let mut out = String::with_capacity(s.len());
    let mut capitalize = true;
    for c in s.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            out.extend(c.to_uppercase());
            capitalize = false;
        } else {
            out.push(c);
        }
    }
    Ident::new(&out, ident.span())
}

/// PascalCase → snake_case, matching bon's builder-module naming
/// (`EllipseBuilder` → `ellipse_builder`).
fn pascal_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.char_indices() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
