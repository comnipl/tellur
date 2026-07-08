# tellur レイアウトシステム チュートリアル

tellur のレイアウトシステムを、設計思想からステップバイステップで理解するためのドキュメントです。「どのコンテナを使えばいいのか」が迷わず選べるようになることをゴールにしています。

対応モジュール: `tellur-core/src/layout/`・`layer.rs`・`fragment.rs`・`placement.rs`・`clip.rs`・`geometry.rs`

姉妹編: [時間システム チュートリアル](./time-tutorial.ja.md)

> 本書は [layout-tutorial.md](./layout-tutorial.md)（英語・正本）の日本語版です。内容が食い違う場合は英語版を優先してください。

## 0. 全体像 — 2つの世界

tellur のレイアウトは、同じコンポーネントプロトコルの上に乗った **2つの世界** でできています。

| | キャンバス世界 | フロー世界 |
|---|---|---|
| 比喩 | After Effects のコンポジション | CSS flexbox / Figma の Auto Layout |
| 座標の決まり方 | 作者が絶対座標で指定する | 親が制約を渡し、子がサイズを答える |
| 主な登場人物 | `Layer`, `Positioned`, `Fragment` | `Frame`, `Flex`, `Padding`, `DecoratedBox`, `SizedBox` |
| 向いている場面 | モーショングラフィックスの演出配置 | 字幕バー、HUD、ランキング表などの「UI 的」な構造 |

どちらの世界の住人も `VectorComponent`（または `RasterComponent`）なので、**自由に入れ子にできます**。「キャンバスに置いた箱の中はフローで組む」「フローで並べたセルの中に小さなキャンバスを置く」のどちらも自然に書けます。

## 1. 土台 — レイアウトプロトコル

すべてのコンポーネントは 2 パスのプロトコルに従います。

```rust
// Pass 1: the parent hands down constraints; the child answers its size.
fn layout(&self, constraints: Constraints) -> Vec2;

// Pass 2: the parent fixes the size; the child renders at exactly that size.
fn render(&self, size: Vec2) -> VectorGraphic;
```

`Constraints` は「このサイズ範囲の中で答えなさい」という親からの封筒です。

```rust
Constraints::tight(size)   // exactly this size
Constraints::loose(max)    // anywhere from zero up to max
Constraints::UNBOUNDED     // no upper bound — answer your intrinsic size
```

原則は「**制約は下りる・サイズは上がる・位置は親が決める**」。子は自分の位置を知りません。位置は常に親が決めます。

もうひとつの原則は「**見えないものは描かれない**」。shape は不可視（fill/stroke の alpha が 0・サイズ 0）のとき、`Text` は空文字や不可視 fill のとき、`Transformed` は opacity 0 のとき、自分で空を描きます。呼び出し側に「alpha が 0 なら出さない」のような可視性ガードは不要です。

> **footgun ①**: `SizeMode::Fill` は親の max 制約を取りますが、max が無限（`UNBOUNDED`）のときは **0 に潰れます**。「Fill したのに消えた」ときは、親が無限制約を渡していないか疑ってください（`Fragment` の中や、`place_at` / `anchored()` で置かれた子など — キャンバス世界は子を無限制約で測ります）。

## 2. キャンバス世界 — `Layer` に置く

固定サイズのキャンバスを作り、絶対座標で置く。モーショングラフィックスの基本形です。

```rust
Layer::builder()
    .size(Vec2(1920.0, 1080.0))
    .child(background.place_at(Vec2::ZERO))
    .child(title.anchored(Anchor::CENTER).snap_to(Vec2(960.0, 400.0)))
    .build()
```

置き方は 2 通りの fluent API があります。

- `.place_at(pos)` — コンポーネントの左上を `pos` に置く
- `.anchored(anchor).snap_to(point)` — コンポーネント上のアンカー点を、キャンバス上の点にスナップする

`anchored().snap_to()` は幾何語彙（`Vec2::anchored` → `AnchoredSize::snap_to`）をそのまま componentに持ち上げたもので、「**子のどこを・どこに**」を読み下せるのが特徴です。どちらも `Positioned` という普通のコンポーネントを返すだけなので、特別な「placed の世界」はありません。

置かれた子は**固有サイズ（無限制約での測定結果）のまま**描かれます。キャンバスは子にサイズを押し付けないので、キャンバスより大きい円を置いても潰れません — はみ出しはクリップの仕事です。

`Layer` のサイズは**必須**です。「子に合わせて縮むグループ」が欲しいときは次の `Fragment` を使います。

## 3. `Fragment` — 透明なグループ

`Fragment` は React の `<>...</>` に相当する、**何も足さないグループ**です。

```rust
// Group several siblings into one component.
Fragment::builder()
    .child(glow.place_at(Vec2(-20.0, -20.0)))
    .child(core_shape.place_at(Vec2::ZERO))
    .build()

// The "render nothing" form (React's `null`).
Fragment::empty()
```

- 子の座標を一切ずらさない（恒等変換）
- 自分のサイズは子の paint bounds のバウンディングボックスに auto-fit
- 空の `Fragment` は「何も描かない」を表す

`Positioned`（1つをずらす）と `Fragment`（複数まとめる／無）で、「なし・1つ・複数・位置つき」のすべてがコンポーネントとして表現できる — これが tellur の React 的な核です。

## 4. フロー世界① — `Frame` でサイズと寄せを決める

`Frame` はフロー世界の「サイズ宣言＋アンカー寄せ」を一手に引き受けるコンテナです。軸ごとに `SizeMode` で外形を決め、その中に子を `Alignment` で寄せます。

```rust
pub enum SizeMode {
    Fill,        // take the parent's max (CSS width: 100%)
    Hug,         // shrink-wrap the child (default)
    Fixed(f32),  // exactly this many logical units
}
```

デフォルトは「両軸 `Hug`・左上寄せ」、つまり**何もしない透明な箱**なので、変えたいものだけ書きます。

```rust
// Width fills the parent, height is fixed; child centered.
Frame::builder()
    .width(SizeMode::Fill)
    .height(SizeMode::Fixed(60.0))
    .align(Anchor::CENTER)
    .child(circle)
    .build()
```

`.align()` には 2 つの形があります。

```rust
// Symmetric: the same anchor on both boxes (the common case).
.align(Anchor::CENTER)             // centering
.align(Anchor::BOTTOM_RIGHT)       // pin to the bottom-right corner

// Asymmetric: snap the child's anchor onto a different box anchor.
.align(Anchor::CENTER.to(Anchor::new(rx, 0.5)))
```

`Anchor::CENTER.to(...)` は「子の中心を、箱の `(rx, 0.5)` 地点に」と読みます。`rx` を時間で動かせば、`Frame` の中を子が滑っていくアニメーションになります（`timeline_to_mp4` の `BouncingDot` がこのパターン）。

> キャンバス世界の `anchored().snap_to(point)` が「**点**へのスナップ」なのに対し、`Frame.align` は「**相対位置**へのスナップ」です。前者は絶対座標、後者は箱のサイズに追従します。

## 5. フロー世界② — `Flex` で並べる

`Flex` は CSS flexbox を意識した一列配置コンテナです。「重ねる」は `Layer`/`Fragment` の仕事で、`Flex` は並べる専用です。

```rust
Flex::builder()
    .axis(Axis::Vertical)
    .spacing(12.0)
    .main_align(MainAlign::Start)      // justify-content
    .cross_align(CrossAlign::Stretch)  // align-items
    .child(row1)
    .child(row2)
    .build()
```

- **main 軸** = `axis` で指定した並べる方向。`MainAlign` は `Start` / `Center` / `End` / `SpaceBetween` / `SpaceAround` / `SpaceEvenly`
- **cross 軸** = それと直交する方向。`CrossAlign` は `Start` / `Center` / `End` / `Stretch`（`Stretch` は子に tight な cross 制約を渡して引き伸ばします）
- `Flex` 自体の main 軸サイズは「親の max まで広がる（有限なら）／無限なら子の合計に縮む」です。明示したいときは外側を `Frame` で包みます

### grow — 余り空間を重みで分ける

flexbox の `flex-grow` に相当します。`.grow(weight)` を付けた子は、固定サイズの兄弟が取った**残りの main 軸空間**を重み比で受け取ります。

```rust
use tellur_core::layout::VectorFlex; // for .grow() on components

Flex::builder()
    .axis(Axis::Horizontal)
    .child(label)                        // intrinsic size
    .child(Flexible::spacer(1.0))        // empty space that absorbs leftovers
    .child(value.grow(2.0))              // takes 2 shares of the leftover
    .build()
```

- `.grow(w)` はコンポーネントにもビルダーにも生えています（`VectorFlex` / `VectorBuilderFlex` とその raster 版）
- `Flexible::spacer(1.0)` は「伸びる空白」。`MainAlign::End` を使わずに「右寄せグループ」を作れます:

```rust
// left ........ right1 right2
Flex::builder()
    .axis(Axis::Horizontal)
    .child(left)
    .child(Flexible::spacer(1.0))
    .child(right1)
    .child(right2)
    .build()
```

> **footgun ②**: `Flexible` は `Flex` の**直下の子**である必要があります。間に `Padding` などを挟むと grow は無視されます（ラッパーとしては透明に振る舞うだけ）。
>
> **footgun ③**: grow を持つ子がいると余り空間は先に grow が消費するため、`MainAlign` の `Center`/`End`/`Space*` は実質 `Start` に退化します。CSS の `flex-grow` と `justify-content` の関係と同じです。
>
> **footgun ④**: main 軸が無限制約のとき grow は不活性です（分けるべき「余り」が定義できないため）。これも CSS と同じ挙動です。

## 6. フロー世界③ — 余白と装飾

```rust
// CSS-style "padded box with a background".
DecoratedBox::builder()
    .background(Color::rgb_u8(20, 20, 30))
    .child(
        Padding::builder()
            .insets(EdgeInsets::all(16.0))
            .child(content),
    )
    .build()
```

- `Padding` — 子の周りに余白を足す。`EdgeInsets::all / symmetric / only`
- `DecoratedBox` — 子のレイアウトサイズいっぱいに背景（vector 版は border も）を敷く。レイアウトには影響しない。raster 版は paint bounds を自分の箱に固定するので、ドロップシャドウなどの**はみ出しを切り取るクリップ矩形**としても機能する
- `SizedBox` — 固定サイズの空箱。固定スペーサーや領域確保に（伸びるスペーサーは `Flexible::spacer`）
- `Clip` — 子を矩形（または任意パス）で**幾何的に**切り抜く vector コンテナ: `Clip::builder().region(ClipRegion::rect(rect)).child(x)`。レイアウトは子に完全に透過し、描画だけを切る。`DecoratedBox` の「paint bounds を箱に固定する」のとは別物で、こちらは箱の途中だろうと任意パスだろうと、指定した領域で本当に切る

## 7. 実例 — 2つの世界の合流

`tellur-renderer/examples/timeline_to_mp4.rs` のドット 1 トラック分です。

```rust
#[component(raster)]
fn BouncingDot(#[builder(into)] t: LocalTime) -> impl RasterComponent {
    let rx = t.bounce(2.5).linear(0.0, 1.0);
    Frame::builder()
        .width(SizeMode::Fill)            // the track spans the parent width
        .height(SizeMode::Fixed(60.0))    // fixed track height
        .align(Anchor::CENTER.to(Anchor::new(rx, 0.5)))  // time-driven sweep
        .child(circle)
        .build()
}
```

これを `Flex` が縦に等間隔配置し、`Padding` + `DecoratedBox` がシーンを包みます。一方 `tellur-live` のデモシーンは逆に、ほぼ全部がキャンバス世界（`Layer` + `place_at`）で組まれています。**演出は座標で、構造はフローで**。使い分けの感覚はこの 2 つの例を見比べるのが早いです。

## 8. 決定表

| やりたいこと | 使うもの |
|---|---|
| 固定キャンバスに座標で置く | `Layer` + `.place_at()` / `.anchored().snap_to()` |
| 兄弟をまとめる・条件付きで何も出さない | `Fragment` / `Fragment::empty()` |
| 1 つだけ位置をずらす | `.place_at()`（= `Positioned`） |
| サイズを宣言する／箱の中に寄せる | `Frame`（`SizeMode` × `Alignment`） |
| 縦・横に並べる | `Flex` |
| 余り空間を比率で配る・伸びる空白 | `.grow(w)` / `Flexible::spacer(w)` |
| 余白 | `Padding` |
| 背景・枠線・はみ出しクリップ | `DecoratedBox` |
| 固定サイズの空白 | `SizedBox` |
| 矩形・任意パスで切り抜く | `Clip` |
| 回転・拡縮・不透明度（レイアウト不変） | `.transform()` / `.opacity()`（= `Transformed`） |
| アンカーを軸に回転・拡縮（中心回転など） | `.transform_around(Anchor::CENTER, t)` |

## 9. vector と raster の対応

すべてのコンテナは vector / raster の 2 変種を持ち、**同名・同セマンティクス**です。

```rust
use tellur_core::layout::{Frame, Flex, Flexible};          // vector
use tellur_core::layout::raster::{Frame, Flex, Flexible};  // raster
```

ソース上も 1 コンテナ 1 ファイル（`layout/frame.rs` など）に両変種が同居しています。raster 側だけの追加責務は `paint_bounds`（ドロップシャドウ等のはみ出しを含む描画範囲）と、キャッシュとの付き合い方（`Flexible` や `Positioned` は `CachePolicy::Transparent` で子にキャッシュスロットを譲る）です。

例外が2つ: `Clip` は vector 専用（raster で欲しければ切ってから `.rasterize()`）、逆に `.opacity()` は raster 側にも同名で存在します（= `Opacity`。alpha 乗算のみで、`.transform()` / `.transform_around()` は今も vector 専用です）。
