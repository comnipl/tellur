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

use std::sync::OnceLock;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, parse_quote, Attribute, DeriveInput, Field, FnArg, Ident, Item, ItemFn,
    ItemStruct, LitStr, Meta, Pat, PatType, Token, Type,
};

mod keyable;

/// The path to the `tellur-core` crate as the *calling* crate sees it, so the
/// generated code resolves whether the caller depends on `tellur-core` directly
/// or only on the `tellur` facade (which re-exports it as `tellur::core`).
///
/// Resolved once per crate compilation — the calling manifest is fixed for the
/// lifetime of one `rustc` invocation, so the lookup is cached.
pub(crate) fn core() -> TokenStream2 {
    static CORE: OnceLock<String> = OnceLock::new();
    CORE.get_or_init(resolve_core_root)
        .parse()
        .expect("resolved tellur-core path parses as tokens")
}

fn resolve_core_root() -> String {
    use proc_macro_crate::{crate_name, FoundCrate};
    // Prefer a direct `tellur-core` dependency (in-tree crates, the renderer /
    // live examples). `extern crate self as tellur_core` in tellur-core makes the
    // `Itself` form resolve when the macro is used inside tellur-core itself.
    if let Ok(found) = crate_name("tellur-core") {
        return match found {
            FoundCrate::Itself => "::tellur_core".to_owned(),
            FoundCrate::Name(name) => format!("::{name}"),
        };
    }
    // Otherwise fall back to the facade, which re-exports core as `tellur::core`.
    match crate_name("tellur") {
        Ok(FoundCrate::Itself) => "crate::core".to_owned(),
        Ok(FoundCrate::Name(name)) => format!("::{name}::core"),
        // Last resort: emit the bare path so the compile error names the crate.
        Err(_) => "::tellur_core".to_owned(),
    }
}

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
        let core = core();
        match self {
            Kind::Vector => quote!(#core::vector::VectorComponent),
            Kind::Raster => quote!(#core::raster::RasterComponent),
            Kind::Timeline => quote!(#core::timeline_component::TimelineComponent),
        }
    }

    fn builder_trait(self) -> TokenStream2 {
        let core = core();
        match self {
            Kind::Vector => quote!(#core::builder::VectorBuilder),
            Kind::Raster => quote!(#core::builder::RasterBuilder),
            Kind::Timeline => quote!(#core::timeline_component::TimelineBuilder),
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
        let core = core();
        match self {
            Kind::Vector => quote!(#core::vector::VectorGraphic),
            Kind::Raster => quote!(#core::raster::RasterImage),
            // The timeline arm has no single `render` graphic; it emits a full
            // multi-method trait impl directly (see `expand_fn`).
            Kind::Timeline => quote!(()),
        }
    }

    /// The `render` signature and the forwarded argument list.
    fn render_sig(self) -> (TokenStream2, TokenStream2) {
        let core = core();
        match self {
            Kind::Vector => (quote!(size: #core::geometry::Vec2), quote!(size)),
            Kind::Raster => (
                quote!(
                    size: #core::geometry::Vec2,
                    target: #core::raster::Resolution,
                    residency: #core::raster::RasterResidency,
                    ctx: &mut dyn #core::render_context::RenderContext
                ),
                quote!(size, target, residency, ctx),
            ),
            // Unused for the timeline arm; it builds its own method set.
            Kind::Timeline => (quote!(), quote!()),
        }
    }
}

/// Parsed `#[component(...)]` attribute arguments: the mandatory `kind` ident
/// plus an optional `name = "<template>"` display-name override. The template
/// (when present) is kept as a [`LitStr`] so its `{field}` placeholders are
/// expanded later against the component's stored fields.
struct ComponentAttr {
    kind: Kind,
    name_template: Option<LitStr>,
}

impl Parse for ComponentAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let kind_ident: Ident = input.parse()?;
        let kind = match &kind_ident {
            id if id == "vector" => Kind::Vector,
            id if id == "raster" => Kind::Raster,
            id if id == "timeline" => Kind::Timeline,
            _ => {
                return Err(syn::Error::new_spanned(
                    &kind_ident,
                    "expected `vector`, `raster`, or `timeline`",
                ));
            }
        };

        let mut name_template = None;
        // Optional `, name = "..."` follows the kind.
        while input.parse::<Token![,]>().is_ok() {
            // Allow (and ignore) a trailing comma after the kind or arg.
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            if key != "name" {
                return Err(syn::Error::new_spanned(
                    &key,
                    "unknown `#[component]` argument (only `name = \"...\"` is supported)",
                ));
            }
            if name_template.is_some() {
                return Err(syn::Error::new_spanned(&key, "duplicate `name` argument"));
            }
            input.parse::<Token![=]>()?;
            name_template = Some(input.parse::<LitStr>()?);
        }

        Ok(Self {
            kind,
            name_template,
        })
    }
}

/// `#[component(vector)]` / `#[component(raster)]` / `#[component(timeline)]`,
/// each optionally carrying `name = "<template>"`.
#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    let ComponentAttr {
        kind,
        name_template,
    } = match syn::parse::<ComponentAttr>(attr) {
        Ok(parsed) => parsed,
        Err(e) => return e.to_compile_error().into(),
    };
    expand_item(item, kind, name_template)
}

/// Backwards-compatible alias for `#[component(vector)]` on a function.
#[proc_macro_attribute]
pub fn vector_component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_item(item, Kind::Vector, None)
}

/// Backwards-compatible alias for `#[component(raster)]` on a function.
#[proc_macro_attribute]
pub fn raster_component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_item(item, Kind::Raster, None)
}

fn expand_item(item: TokenStream, kind: Kind, name_template: Option<LitStr>) -> TokenStream {
    let item = parse_macro_input!(item as Item);
    let result = match item {
        Item::Fn(func) => expand_fn(func, kind, name_template),
        Item::Struct(s) => {
            if let Some(tpl) = name_template {
                // Struct-form components keep their hand-written trait impls;
                // the display-name hook is function-form only (task scope).
                Err(syn::Error::new_spanned(
                    tpl,
                    "`name = \"...\"` is only supported on function-form `#[component]`s",
                ))
            } else {
                expand_struct(s, kind)
            }
        }
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
    let core = core();
    Ok(quote! {
        #[derive(#core::__bon::Builder)]
        #[builder(derive(Into), crate = #core::__bon)]
        #s

        #glue
    })
}

// ─── function form ───────────────────────────────────────────────────────────

fn expand_fn(func: ItemFn, kind: Kind, name_template: Option<LitStr>) -> syn::Result<TokenStream2> {
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
    let core = core();

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

    // The display name surfaced in the live UI's arrangement tree: either the
    // auto-derived component name, or an interpolated `name = "..."` template.
    // The `#[available]`/`#[clock]` idents are stripped (not stored fields), so
    // referencing them in a template is rejected with a tailored message.
    let stripped_idents: Vec<&Ident> = available_ident.into_iter().chain(clock_ident).collect();
    let name_expr = build_name_expr(
        &struct_ident,
        name_template.as_ref(),
        &field_idents,
        &stripped_idents,
    )?;

    // `#[available]` and `#[clock]` are mutually exclusive injection paths —
    // the former belongs to the raster/vector layout protocol, the latter to
    // the timeline frame protocol — but they never coexist on one fn (the
    // timeline arm has no `#[available]` and the others reject `#[clock]`).
    let (build_fn, trait_impl) = if kind == Kind::Timeline {
        timeline_codegen(
            &struct_ident,
            &trait_path,
            &build_method,
            field_idents.iter().copied(),
            clock_ident.zip(clock_type),
            body,
            &name_expr,
        )
    } else {
        let (render_sig, render_args) = kind.render_sig();
        let graphic_path = kind.graphic();
        let (build_fn, build_call_layout, build_call_render, build_call_paint_bounds) =
            if let (Some(av_ident), Some(av_type)) = (available_ident, available_type) {
                let build_call_render = if kind == Kind::Raster {
                    quote! {
                        let __child = self.#build_method(size);
                        #core::render_context::RenderContext::render(
                            ctx,
                            &__child,
                            size,
                            target,
                            residency,
                        )
                    }
                } else {
                    quote! {
                        let __child = self.#build_method(size);
                        #trait_path::render(&__child, #render_args)
                    }
                };
                (
                    quote! {
                        #[doc(hidden)]
                        fn #build_method(&self, #av_ident: #av_type) -> impl #trait_path + 'static {
                            let Self { #( #field_idents ),* } = ::core::clone::Clone::clone(self);
                            #body
                        }
                    },
                    quote! {
                        let __available = #core::geometry::Vec2(
                            if constraints.max.0.is_finite() { constraints.max.0 } else { 0.0 },
                            if constraints.max.1.is_finite() { constraints.max.1 } else { 0.0 },
                        );
                        let __child = self.#build_method(__available);
                        #trait_path::layout(&__child, constraints)
                    },
                    build_call_render,
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
        let cache_policy_impl = if kind == Kind::Raster {
            if available_ident.is_some() {
                quote! {
                    fn cache_policy(&self) -> #core::render_context::CachePolicy {
                        #core::render_context::CachePolicy::Transparent
                    }
                }
            } else {
                quote! {
                    fn cache_policy(&self) -> #core::render_context::CachePolicy {
                        let __child = self.#build_method();
                        #trait_path::cache_policy(&__child)
                    }
                }
            }
        } else {
            quote! {}
        };
        let trait_impl = quote! {
            impl #trait_path for #struct_ident {
                fn layout(
                    &self,
                    constraints: #core::geometry::Constraints,
                ) -> #core::geometry::Vec2 {
                    #build_call_layout
                }

                fn paint_bounds(
                    &self,
                    size: #core::geometry::Vec2,
                ) -> #core::geometry::Rect {
                    #build_call_paint_bounds
                }

                fn render(&self, #render_sig) -> #graphic_path {
                    #build_call_render
                }

                #cache_policy_impl

                // Surfaces this component's display name to the timeline
                // arrangement tree. For a raster component this flows through the
                // `RasterComponent -> TimelineComponent` blanket; a vector
                // component only reaches the timeline after `.rasterize()`, which
                // does NOT carry this name forward (documented limitation), so the
                // override is harmless there.
                fn arrangement_name(&self) -> ::core::option::Option<::std::string::String> {
                    ::core::option::Option::Some(#name_expr)
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
        #[derive(::core::clone::Clone, #core::__bon::Builder)]
        #[builder(derive(Into), crate = #core::__bon)]
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

/// Builds the `String`-typed expression that yields a component instance's
/// display name at arrangement-time.
///
/// - With no `name = "..."` template: a `String` literal of the component's
///   PascalCase name (e.g. `Backdrop`).
/// - With a template: a [`format!`] over the template's literal text where each
///   `{ident}` placeholder binds to the matching STORED field (`self.ident`).
///   `{{` / `}}` are literal braces. Every `{ident}` is validated at expansion
///   time against `field_idents`; an unknown name — or one of the stripped
///   `#[available]` / `#[clock]` idents (which are not stored fields) — produces
///   a `compile_error!` with a clear message.
fn build_name_expr(
    struct_ident: &Ident,
    name_template: Option<&LitStr>,
    field_idents: &[&Ident],
    stripped_idents: &[&Ident],
) -> syn::Result<TokenStream2> {
    let Some(template) = name_template else {
        let lit = struct_ident.to_string();
        return Ok(quote!(::std::string::String::from(#lit)));
    };

    let raw = template.value();
    let span = template.span();

    // Walk the template, splitting it into a `format!` literal (with `{}` holes)
    // and the ordered list of field idents that fill those holes.
    let mut fmt_literal = String::with_capacity(raw.len());
    let mut placeholders: Vec<Ident> = Vec::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    fmt_literal.push_str("{{");
                    continue;
                }
                // Read the placeholder name up to the closing `}`.
                let mut name = String::new();
                let mut closed = false;
                for nc in chars.by_ref() {
                    if nc == '}' {
                        closed = true;
                        break;
                    }
                    name.push(nc);
                }
                if !closed {
                    return Err(syn::Error::new(
                        span,
                        format!("unterminated `{{` in `name` template; expected a closing `}}` after `{name}`"),
                    ));
                }
                let name = name.trim();
                if name.is_empty() {
                    return Err(syn::Error::new(
                        span,
                        "empty `{}` placeholder in `name` template; name a stored field",
                    ));
                }
                let field = field_idents.iter().find(|f| **f == name);
                if field.is_none() {
                    // Distinguish a stripped `#[available]`/`#[clock]` arg (not
                    // stored, so never interpolable) from a plain typo.
                    if stripped_idents.iter().any(|s| **s == name) {
                        return Err(syn::Error::new(
                            span,
                            format!(
                                "`{{{name}}}` refers to a `#[available]`/`#[clock]` argument, which is not a stored field and cannot be interpolated"
                            ),
                        ));
                    }
                    let known = field_idents
                        .iter()
                        .map(|f| f.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(syn::Error::new(
                        span,
                        format!(
                            "`{{{name}}}` in `name` template does not match any stored field (available fields: {known})"
                        ),
                    ));
                }
                fmt_literal.push_str("{}");
                placeholders.push(Ident::new(name, span));
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    fmt_literal.push_str("}}");
                } else {
                    return Err(syn::Error::new(
                        span,
                        "stray `}` in `name` template; write `}}` for a literal brace",
                    ));
                }
            }
            other => fmt_literal.push(other),
        }
    }

    Ok(quote! {
        ::std::format!(#fmt_literal #( , self.#placeholders )*)
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
    clock: Option<(&Ident, &Type)>,
    body: &syn::Block,
    name_expr: &TokenStream2,
) -> (TokenStream2, TokenStream2) {
    let destructure_idents: Vec<&Ident> = field_idents.collect();
    let core = core();

    // The body is built either with the real clock (when `#[clock]` is present)
    // or with no clock at all. `__tellur_build` takes a clock by value in the
    // first case so every delegator forwards exactly the clock it has.
    let (build_fn, build_with_clock, build_structural) = if let Some((ck_ident, ck_type)) = clock {
        (
            quote! {
                #[doc(hidden)]
                fn #build_method(&self, #ck_ident: #ck_type) -> impl #trait_path + 'static {
                    let Self { #( #destructure_idents ),* } = ::core::clone::Clone::clone(self);
                    #body
                }
            },
            quote!(self.#build_method(clock)),
            quote!(self.#build_method(#core::timeline_component::Clock::structural())),
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
                out: &mut #core::timeline_component::ResolveCtx,
            ) -> f32 {
                let __child = #build_structural;
                #trait_path::resolve(&__child, abs_start, out)
            }

            fn frame(
                &self,
                clock: #core::timeline_component::Clock<'_>,
                canvas: #core::geometry::Vec2,
                target: #core::raster::Resolution,
                residency: #core::raster::RasterResidency,
                ctx: &mut dyn #core::render_context::RenderContext,
            ) -> ::core::option::Option<#core::raster::RasterImage> {
                let __child = #build_with_clock;
                #trait_path::frame(
                    &__child,
                    clock,
                    canvas,
                    target,
                    residency,
                    ctx,
                )
            }

            fn samples(
                &self,
                clock: #core::timeline_component::Clock<'_>,
                window: f32,
            ) -> ::core::option::Option<#core::timeline_component::AudioBuffer> {
                let __child = #build_with_clock;
                #trait_path::samples(&__child, clock, window)
            }

            fn mix_into(
                &self,
                mix: &mut #core::audio::AudioMix,
                start_secs: f32,
                speed: f32,
            ) {
                // Delegate the eager audio mix-down to the body, exactly like
                // `cues`/`arrangement`. Without this the generated impl falls back
                // to the trait's silent default, muting any fn-form component that
                // composes audio (e.g. a `Dialogue(voice: AudioFile)`).
                let __child = #build_structural;
                #trait_path::mix_into(&__child, mix, start_secs, speed);
            }

            fn cues(&self, offset: f32) -> ::std::vec::Vec<#core::timeline_component::Cue> {
                let __child = #build_structural;
                #trait_path::cues(&__child, offset)
            }

            fn arrangement(&self, offset: f32) -> #core::timeline_component::Arrangement {
                // Relabel the delegated node IN PLACE: build the child node, then
                // stamp this component's display name onto it. No extra tree level
                // is introduced — the inner kind/interval/children are preserved.
                let __child = #build_structural;
                let mut __node = #trait_path::arrangement(&__child, offset);
                __node.name = ::core::option::Option::Some(#name_expr);
                __node
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
    let core = core();
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
                impl<__S> #core::builder::Effect for #builder_ty<__S>
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

        // TIMELINE kind only: the child setter captures its `.child(...)` call
        // site (`#[track_caller]` + `Location::caller()`) and wraps the boxed
        // child in a `Sourced` so each arrangement node can be traced back to its
        // authoring line. Raster/vector kinds keep the plain push UNCHANGED —
        // `Sourced` is a `TimelineComponent`-only decorator.
        let push_one = |child: TokenStream2| {
            if kind == Kind::Timeline {
                quote! {
                    self.#field.push(::std::boxed::Box::new(
                        #core::timeline_component::Sourced::new(
                            ::core::panic::Location::caller(),
                            ::core::convert::Into::into(#child),
                        ),
                    ));
                }
            } else {
                quote! {
                    self.#field.push(::core::convert::Into::into(#child));
                }
            }
        };
        // `#[track_caller]` so `Location::caller()` resolves to the user's
        // `.child(...)` line; harmless (and omitted) for non-timeline kinds.
        let track_caller = if kind == Kind::Timeline {
            quote!(#[track_caller])
        } else {
            quote!()
        };
        let push_each = push_one(quote!(child));
        // The plural extend setters share ONE call line for every item; the
        // timeline kind wraps each item with that same location.
        let extend_field = if kind == Kind::Timeline {
            quote! {
                let __loc = ::core::panic::Location::caller();
                self.#field.extend(children.into_iter().map(|__c| {
                    let __boxed: #box_dyn = ::std::boxed::Box::new(
                        #core::timeline_component::Sourced::new(
                            __loc,
                            ::core::convert::Into::into(__c),
                        ),
                    );
                    __boxed
                }));
            }
        } else {
            quote! {
                self.#field
                    .extend(children.into_iter().map(::core::convert::Into::into));
            }
        };

        let each_method = c.each.as_ref().map(|each| {
            let maybe_each = format_ident!("maybe_{}", each);
            quote! {
                #track_caller
                pub fn #each(mut self, child: impl ::core::convert::Into<#item_ty>) -> Self {
                    #push_each
                    self
                }

                /// Pushes one child, or nothing when `child` is `None`.
                #track_caller
                pub fn #maybe_each(
                    mut self,
                    child: ::core::option::Option<impl ::core::convert::Into<#item_ty>>,
                ) -> Self {
                    if let ::core::option::Option::Some(child) = child {
                        #push_each
                    }
                    self
                }
            }
        });
        quote! {
            impl<__S: #state_mod::State> #builder_ty<__S> {
                #each_method

                #track_caller
                pub fn #field<__I, __T>(mut self, children: __I) -> Self
                where
                    __I: ::core::iter::IntoIterator<Item = __T>,
                    __T: ::core::convert::Into<#item_ty>,
                {
                    #extend_field
                    self
                }

                /// Extends with the iterator, or nothing when `children` is `None`.
                #track_caller
                pub fn #maybe_field<__I, __T>(
                    mut self,
                    children: ::core::option::Option<__I>,
                ) -> Self
                where
                    __I: ::core::iter::IntoIterator<Item = __T>,
                    __T: ::core::convert::Into<#item_ty>,
                {
                    if let ::core::option::Option::Some(children) = children {
                        #extend_field
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
