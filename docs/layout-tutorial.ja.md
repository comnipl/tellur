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
| 主な登場人物 | `Layer`, `Fragment`, Point target の `Positioned` | `Frame`, `Flex`, `Stack`, `Padding`, `DecoratedBox`, `SizedBox`, Anchor target の `Positioned` |
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

> **footgun ①**: `SizeMode::Fill` は親の max 制約を取りますが、max が無限（`UNBOUNDED`）のときは **0 に潰れます**。「Fill したのに消えた」ときは、親が無限制約を渡していないか疑ってください。

## 2. キャンバス世界 — `Layer` に置く

固定サイズのキャンバスを作り、絶対座標で置く。モーショングラフィックスの基本形です。

```rust
Layer::builder()
    .size(Vec2(1920.0, 1080.0))
    .child(background.place_at(Vec2::ZERO))
    .child(title.anchored(Anchor::CENTER).snap_to(Vec2(960.0, 400.0)))
    .child(badge.anchored(Anchor::TOP_RIGHT).snap_to(Anchor::TOP_RIGHT))
    .build()
```

`Positioned` は絶対配置と box 相対配置を同じ文で表します。「子のこの anchor
を target へ合わせ、その後で offset する」です。

- `.place_at(pos)` — コンポーネントの左上を `pos` に置く
- `.anchored(child_anchor).snap_to(point)` — 絶対座標の `Vec2` へスナップする
- `.anchored(child_anchor).snap_to(parent_anchor)` — 親 box 上の割合位置へスナップする
- `.offset(Vec2(dx, dy))` — どちらのスナップでも、最後に一定の px 平行移動を足す

target は `SnapTarget` で表され、`Vec2` は `SnapTarget::Point`、`Anchor` は
`SnapTarget::Anchor` へ変換されます。したがって `anchored().snap_to()` は 2 つの
レイアウト世界で文法を変えず、「**子のどこを・どこに**」と左から右へ読めます。
どの形も普通の `Positioned` を返すだけで、特別な「placed の世界」はありません。

Point target は **out-of-flow** です。子を無限制約で測って固有サイズを名乗るので、
キャンバスより大きい円も潰れません。一方 Anchor target は親から渡された有限の
max を名乗り、その box の中で実際の子を loose に測り、描画時に解決済み box
サイズから target を計算します。これは `SizeMode::Fill` と同じ fill 規則です。

```rust
chip
    .anchored(Anchor::CENTER_LEFT)       // point on the child
    .snap_to(Anchor::TOP_LEFT)           // point on the parent box
    .offset(Vec2(28.0, 0.0))             // final pixel nudge
```

> **footgun ②**: `SnapTarget::Anchor` は、親の max が `UNBOUNDED` の軸で
> `0` に縮退します。`SizeMode::Fill` と同族の挙動です。`Layer`・`Frame`・`Stack`
> のような有限 box の下で使い、auto-fit する `Fragment` 内で out-of-flow に
> 置くときは `Vec2` の Point target を使ってください。

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

`Positioned`（1つを配置する）と `Fragment`（複数まとめる／無）で、「なし・1つ・複数・位置つき」のすべてがコンポーネントとして表現できる — これが tellur の React 的な核です。

## 4. フロー世界① — サイズは `Frame`、配置は `Positioned`

`Frame` は意図的に小さく保たれたサイズ決定コンテナです。軸ごとに `SizeMode`
で外形を決め、子は左上に置きます。その box 内で子を寄せたいときは、キャンバスと
同じ `Positioned` の語彙で子を包みます。

```rust
pub enum SizeMode {
    Fill,        // take the parent's max (CSS width: 100%)
    Hug,         // shrink-wrap the child (default)
    Fixed(f32),  // exactly this many logical units
}
```

デフォルトは両軸 `Hug` です。通常の `Frame` は透明な sizing wrapper なので、
変えたいものだけ書きます。

```rust
// Width fills the parent, height is fixed; the child is centered in that box.
Frame::builder()
    .width(SizeMode::Fill)
    .height(SizeMode::Fixed(60.0))
    .child(
        circle
            .anchored(Anchor::CENTER)
            .snap_to(Anchor::CENTER),
    )
    .build()
```

対称な寄せも非対称な寄せも同じ式で書けます。

```rust
// The same anchor on both boxes: centering or corner-pinning.
child.anchored(Anchor::CENTER).snap_to(Anchor::CENTER)
child.anchored(Anchor::BOTTOM_RIGHT).snap_to(Anchor::BOTTOM_RIGHT)

// Different anchors: the child's center follows a proportional target.
child.anchored(Anchor::CENTER).snap_to(Anchor::new(rx, 0.5))
```

`rx` を時間で動かせば子が box 内を滑ります。`timeline_to_mp4` の
`BouncingDot` がこのパターンです。Anchor target の `Positioned` は渡された両軸を
Fill するため、有限の参照 box の下へ置いてください。特に `Hug` 軸を持つ `Frame`
の子として包むと、その軸は Hug ではなく Fill になります。子の固有サイズから
決めたい軸では、包んでいない子を使います。

## 5. フロー世界② — `Flex` で並べる

`Flex` は CSS flexbox を意識した一列配置コンテナです。重ねるなら `Layer` / `Stack`、並べるなら `Flex` です。

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

> **footgun ③**: `Flexible` は `Flex` の**直下の子**である必要があります。間に `Padding` などを挟むと grow は無視されます（ラッパーとしては透明に振る舞うだけ）。
>
> **footgun ④**: grow を持つ子がいると余り空間は先に grow が消費するため、`MainAlign` の `Center`/`End`/`Space*` は実質 `Start` に退化します。CSS の `flex-grow` と `justify-content` の関係と同じです。
>
> **footgun ⑤**: main 軸が無限制約のとき grow は不活性です（分けるべき「余り」が定義できないため）。これも CSS と同じ挙動です。

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

### `Stack` — 1 つの base にサイズを合わせて重ねる

`Stack` は「content が決めたサイズに装飾や overlay を合わせる」ためのコンテナです。
3 種類の slot を持ち、配置文法そのものは意図的に持ちません。

- ちょうど 1 個の `.base(child)` だけが Stack のレイアウトサイズを決める
- 0 個以上の `.under(child)` は base の背後へ描く
- 0 個以上の `.over(child)` は base の前面へ描く
- `.maybe_under(option)` / `.maybe_over(option)` で任意の layer を足す

`base` は builder の必須 field なので、指定するまで `.build()` は完成しません。

```rust
Stack::builder()
    .under(decorations) // receives the base size as tight constraints
    .base(
        Padding::builder()
            .insets(EdgeInsets::all(16.0))
            .child(content),
    )
    .over(
        chip
            .anchored(Anchor::CENTER_LEFT)
            .snap_to(Anchor::TOP_LEFT)
            .offset(Vec2(28.0, 0.0)),
    )
    .build()
```

Stack は受け取った constraints をそのまま base へ渡し、base の答えを自分のサイズに
します。under / over の各子は、その解決済みサイズの tight constraints で layout
されます。したがって `SizeMode::Fill`・`#[available]`・`SnapTarget::Anchor` はすべて
同じ box を見ます。描画順は under の宣言順、base、over の宣言順です。

under / over はレイアウトサイズに影響しませんが、その完全な paint bounds は Stack
の paint bounds へ union されます。offset shadow・辺から張り出す chip・outset stroke
もラスタライズ時にクリップされません。辺相対配置や offset は各 slot 内で
`Positioned` を使って表します。

## 7. 実例 — 2つの世界の合流

`tellur-renderer/examples/timeline_to_mp4.rs` のドット 1 トラック分です。

```rust
#[component(raster)]
fn BouncingDot(#[builder(into)] t: LocalTime) -> impl RasterComponent {
    let rx = t.bounce(2.5).linear(0.0, 1.0);
    Frame::builder()
        .width(SizeMode::Fill)            // the track spans the parent width
        .height(SizeMode::Fixed(60.0))    // fixed track height
        .child(
            circle
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(rx, 0.5)), // time-driven sweep
        )
        .build()
}
```

これを `Flex` が縦に等間隔配置し、`Padding` + `DecoratedBox` がシーンを包みます。一方 `tellur-live` のデモシーンは逆に、ほぼ全部がキャンバス世界（`Layer` + `place_at`）で組まれています。**演出は座標で、構造はフローで**。使い分けの感覚はこの 2 つの例を見比べるのが早いです。

## 8. 決定表

| やりたいこと | 使うもの |
|---|---|
| 固定キャンバスに座標で置く | `Layer` + `.place_at()` / `.snap_to(Vec2)` |
| 解決済み box に相対配置する | `.anchored(child_anchor).snap_to(parent_anchor)` |
| 兄弟をまとめる・条件付きで何も出さない | `Fragment` / `Fragment::empty()` |
| スナップ後に px 単位でずらす | `Positioned` の `.offset(Vec2)` |
| サイズを宣言する | `Frame` + `SizeMode` |
| content と同じサイズで背面・前面へ重ねる | `Stack`（`under` / `base` / `over`） |
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
use tellur_core::layout::{Frame, Flex, Flexible, Stack};          // vector
use tellur_core::layout::raster::{Frame, Flex, Flexible, Stack};  // raster
```

ソース上も 1 コンテナ 1 ファイル（`layout/frame.rs` など）に両変種が同居しています。raster 側だけの追加責務は `paint_bounds`（ドロップシャドウ等のはみ出しを含む描画範囲）と、キャッシュとの付き合い方（`Flexible` や `Positioned` は `CachePolicy::Transparent` で子にキャッシュスロットを譲る）です。

例外が2つ: `Clip` は vector 専用（raster で欲しければ切ってから `.rasterize()`）、逆に `.opacity()` は raster 側にも同名で存在します（= `Opacity`。alpha 乗算のみで、`.transform()` / `.transform_around()` は今も vector 専用です）。
