<h1 align="center">Tellur</h1>

<p align="center">
  <a href="https://crates.io/crates/tellur"><img src="https://img.shields.io/crates/v/tellur.svg" alt="crates.io"></a>
  <a href="https://docs.rs/tellur"><img src="https://img.shields.io/docsrs/tellur" alt="docs.rs"></a>
  <a href="https://github.com/comnipl/tellur/actions/workflows/rust.yml"><img src="https://github.com/comnipl/tellur/actions/workflows/rust.yml/badge.svg" alt="Rust Checks"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
</p>

<p align="center">
  <i>A video editing and motion graphics library made with Rust</i>
</p>

<p>
  <a href="README.ja.md">日本語(Japanese)</a>
</p>

<hr />

<p align="center">
  <img src="docs/assets/hero-mathematics.gif" alt="" width="354">
  <img src="docs/assets/hero-kinetic-motion.gif" alt="" width="354">
</p>

Tellur is a component-oriented video editing library.
It aims to be a general-purpose library capable of handling various video styles.
- Rich expressions like motion graphics
- Variety-show style telops/captions
- Calm tones suitable for explainer videos, etc.

Each element in a video is represented by a combination of components as shown below.

```rust
#[component(vector)]
fn Dot(center: Vec2, radius: f32, color: Color) -> impl VectorComponent {
    Circle::builder()
        .radius(radius)
        .fill(Fill {
            paint: Paint::Solid(color),
        })
        .anchored(Anchor::CENTER)
        .snap_to(center)
}
```

Components are functional and pure, enabling caching at the subtree level.

Additionally, GPU rendering is implemented for most built-in components, making it extremely fast.


## Comparison with Similar Tools

**[Remotion](https://www.remotion.dev/)** (React)

- Pros: Since it has its own rasterizer, it is easy to create rich expressions that go beyond the limitations of HTML/CSS.
- Cons: The writing style cannot compete with the conciseness of JSX.

**[Manim](https://www.manim.community/)** (Python)

- Pros: Rendering is fast, and it provides a full suite of tools for video production regardless of genre, including timelines, layouts, and live previews.
- Cons: Complex mathematical expressions are still immature.


## Getting Started

You can create a timeline project from a template using `tellur create`. This generates a `Cargo.toml` (a `cdylib` crate) and sample scenes. If executed within a Cargo workspace, it also automatically handles member registration and `tellur` dependencies.

```console
$ tellur create my-video
```

You can specify the display name of the timeline with `--title` (defaults to the directory name if omitted).

## Live Preview

`tellur live` provides a browser preview with hot-rebuilding of the project.

```console
$ tellur live --project path/to/your-video --gpu
```

<p align="center">
  <img src="docs/assets/live.png" alt="tellur live preview in browser" width="720">
</p>

## Crate Structure

| crate | Role |
|---|---|
| [`tellur`](https://crates.io/crates/tellur) | Facade. Re-exports everything below. `tellur` CLI (`cli` feature) |
| [`tellur-core`](https://crates.io/crates/tellur-core) | Component model, layout, easing, events, text, LaTeX math |
| [`tellur-renderer`](https://crates.io/crates/tellur-renderer) | GPU/CPU rasterization and ffmpeg encoding |
| [`tellur-macros`](https://crates.io/crates/tellur-macros) | `#[component]` and `#[derive(Keyable)]` |
| [`tellur-live`](https://crates.io/crates/tellur-live) | Live preview server |
| [`tellur-plugin`](https://crates.io/crates/tellur-plugin) | Dynamic library ABI used by live preview |

## Requirements

- **ffmpeg**: Required for encoding.
- **fontconfig** (Linux only): Required for font detection.

## Documentation

- [Layout system tutorial](docs/layout-tutorial.md) — the canvas and flow worlds: `Layer`, `Frame`, `Flex`, anchors, and clipping ([日本語](docs/layout-tutorial.ja.md))
- [Time system tutorial](docs/time-tutorial.md) — `Phase`, `Window`, events, and the placement-clock world ([日本語](docs/time-tutorial.ja.md))

For AI coding agents, an [authoring skill](skills/tellur-authoring/SKILL.md) captures the idiomatic "Tellur style" on top of the tutorials.

A getting-started tutorial is under preparation.

## License

MIT
