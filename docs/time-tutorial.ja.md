# tellur 時間システム チュートリアル

tellur の時間システムを、設計思想からステップバイステップで理解するためのドキュメントです。「時間から値を作るとき、どの型を使えばいいのか」が迷わず選べるようになることをゴールにしています。

対応モジュール: `tellur-core/src/time.rs`・`phase.rs`・`window.rs`・`easing.rs`・`interpolate.rs`・`timeline_component/`・`timeline_container/`

姉妹編: [レイアウトシステム チュートリアル](./layout-tutorial.ja.md)

> 本書は [time-tutorial.md](./time-tutorial.md)（英語・正本）の日本語版です。内容が食い違う場合は英語版を優先してください。

## 0. 全体像 — 2つの時間世界

レイアウトに「キャンバス世界とフロー世界」があったように、時間にも **2つの世界** があります。

| | 絶対時刻の世界 | 配置クロックの世界 |
|---|---|---|
| 時間の入手方法 | `Timeline::build` が渡す `TimelineTime` | `#[clock]` が注入する `Clock` |
| 座標の決まり方 | 作者がタイムライン上の絶対秒で指定する | コンテナが `.at(..)` の配置から割り当てる |
| 主な登場人物 | `Time` / `Phase` / `Window` | `Clock`, `Timeline`, `Sequence`, `Placed`, `Event` |
| 向いている場面 | 一本物のモーショングラフィックス演出 | クリップを並べ替え・差し替えする構成的な動画 |

空間側との対応もきれいに揃っています。

| 空間（レイアウト） | 時間（このドキュメント） |
|---|---|
| `Layer`（重ねる） | `Timeline`（時間上に重ねる） |
| `Flex`（カーソルで並べる） | `Sequence`（前から順に並べる) |
| `Positioned`（1つを置く） | `Placed`（`.at(..)` の結果） |
| `SizedBox`（固定サイズの空白） | `TimeBox`（固定長の間・§7） |

どちらの世界でも、時間から値を作る語彙は共通です: **`Time` → `Phase` / `Window` → eased な値**。

## 1. 基本パイプライン — `Time` → `Phase` → 値

時間駆動アニメーションの主力はこの1行です。

```rust
// Progress through [0.55s, 1.2s], eased, mapped into [0.0, 1.0].
let hero_in = t.phase(0.55, 1.2).ease_out_cubic(0.0, 1.0);
```

3段階に分解すると:

1. **`Time`** — 「何秒か」だけを知っている型。`TimelineTime`（タイムライン絶対時刻）と `LocalTime`（再基底化されたローカル時刻）の2つがあり、コンビネータはすべて共通です
2. **`Phase`** — `[0.0, 1.0]` に検証済みの進捗スカラー。`t.phase(start, end)` は区間を単位区間へ線形写像し、外側では**飽和**（0 か 1 に張り付く）します
3. **`PhaseEasing`** — `phase.ease_*(from, to)` がカーブを当てて目的の量（alpha・半径・座標…）まで一気に持っていきます。`linear` / `ease_smoothstep` / `ease_out_cubic` / `ease_out_quint` / `ease_in_out_quint` / `ease_in_out_expo` は `[from, to]` に収まり、`ease_in_back` / `ease_out_elastic` は意図的にはみ出します（その「行き過ぎ」が演出です）。名前付きカーブで足りないときは `ease_bezier(x1, y1, x2, y2, from, to)` — CSS の `cubic-bezier` と同じ 4 制御点でカーブを自作できます（`y1`/`y2` を単位区間の外に置けば、これも意図的にはみ出せます）

タイムラインの秒数は、component・renderer・live preview の境界まで一貫して `f64` です。`Phase` と描画値は `f32` のままです。つまり、sample/frame 位置の精度は秒数として保ち、単位区間の進捗や描画値になった境界でだけ狭めます。

逆方向のアニメーションは `(from, to)` を入れ替えるだけです。

```rust
let fade_out = t.phase(1.7, 2.15).ease_in_back(1.0, 0.0);  // 1 → 0
```

> **footgun ①**: `phase` / `window` は `end > start` の有限区間を要求し、破ると panic します。ゼロ幅の区間に意味のある進捗は定義できないためです。

## 2. `Phase` — 純粋な進捗スカラー

`Phase` は「単位区間に検証済みの f32」以上のことを何も知りません。秒・区間・カーソルの知識は一切持たず、それらは次節の `Window` の仕事です。

```rust
Phase::new(0.5)        // Some(Phase) — validating
Phase::saturating(2.0) // Phase(1.0) — clamping
phase.get()            // the inner f32, guaranteed in [0, 1]
phase.map(|x| 4.0 * x * (1.0 - x))  // custom value-space remap (hat curve)
```

小ささには理由があります。`Phase` は値のビットパターンで `Eq`/`Hash` されるので、**コンポーネントのフィールド（= レンダーキャッシュのキー）にそのまま使えます**。飽和する性質と合わせると「アニメーションが終わったらフレーム間でキーが一致し、キャッシュが当たり続ける」が自然に成立します（§4）。

## 3. `Window` — 区間を覚えている視点

`Phase` は便利ですが、写像した瞬間に区間を忘れます。「窓が閉じるまであと何秒？」「窓が開いてから累計何秒？」に答えるには、区間とカーソルを保持したままの視点が要ります。それが `Window` です。

```rust
let radar = time.window(3.95, 5.4);

radar.phase()      // the saturating Phase view (0 → 1 inside the window)
radar.elapsed()    // seconds since start — keeps counting PAST the end
radar.remaining()  // countdown to the end, clamped at 0
radar.before()     // seconds until the window opens
radar.after()      // seconds past the close
radar.is_inside()  // the same gate as t.during(a, b)
```

`phase()` と `elapsed()` を1つの宣言された区間に束ねられるのが肝です。たとえば「フェードインしながら、開始時点から回転し続けるレーダー」:

```rust
let radar = time.window(3.95, 5.4);
let opacity = radar.phase().ease_out_cubic(0.0, 1.0);
let angle = radar.elapsed() * 2.4;   // keeps accruing while visible
```

> **footgun ②**: `elapsed()` / `after()` は窓が閉じても止まりません。それが存在理由です（「イントロが終わってから5秒後」は飽和する `Phase` では表現できません）。止まってほしいときは `remaining()` か `clamped()`（§4）を使います。

### `sub_secs` — 窓ローカル秒でサブイベントを刻む

1つの窓の中に複数のサブイベントをスタッガーさせたいとき、`sub_secs(range)` が「窓の開始から数えた秒範囲」を新しい `Window` として切り出します。カーソルを持っているので**全域関数**です — 失敗しません。

```rust
let reveal = time.window(0.05, 1.332);

// The i-th horizon line slides in over [i*8ms, 0.4s + i*8ms] of the reveal.
let line_in = reveal
    .sub_secs((i as f64 * 0.008)..(0.4 + i as f64 * 0.008))
    .ease_in_out_expo(0.0, 1.0);
```

`PhaseEasing` は `Window` にも実装されているので、`.phase()` を挟まずそのまま `.ease_*(from, to)` まで書けます。

### `envelope` — 出現と退場をワンセットで

```rust
// Rise over the first 0.3s, hold, fall over the last 0.5s.
let alpha = time.window(2.0, 6.0).envelope(0.3, 0.5).get();
```

字幕やテロップのような「現れて、留まって、消える」をひとことで表せます。もっと複雑な形（カーブ違いの rise/fall など）は、§1 のイディオムどおり **因子の積** で組みます: `rise * fall` はどちらも `[0, 1]` の f32 なので、掛け算が「両方の条件を満たすときだけ見える」を意味します。

## 4. コンポーネント境界を越える — `clamped()` スナップショット

時間駆動の値をコンポーネントの**フィールド**として渡すとき、その値はキャッシュキーの一部になります。ここで型の選択がキャッシュ効率を決めます。

- **`Phase`** — 飽和したら一定。アニメーション完了後はフレーム間でキーが一致し、キャッシュが当たります
- **生の `Window`** — カーソルが毎フレーム動くので、**キーが毎フレーム変わりキャッシュが全滅します**
- **`Window::clamped()`** — カーソルを `[start, end]` に切り詰めたスナップショット。窓の手前では `(start, end, start)`、飽和後は `(start, end, end)` で一定になり、`Phase` と同じキャッシュ安定性を取り戻します

```rust
// The component receives a saturating snapshot, not the live cursor.
Backdrop::builder()
    .reveal(t.window(REVEAL_START, REVEAL_END).clamped())
    .build()

#[component(vector)]
pub fn Backdrop(reveal: Window, palette: Palette) -> impl VectorComponent {
    // sub_secs staggering still works — and freezes once the reveal saturates.
    let ring_in = reveal.sub_secs(0.55..1.05).ease_in_out_expo(0.0, 1.0);
    // ...
}
```

使い分けの目安: **その場で値まで潰すなら `phase`、サブイベントや経過秒が要るなら `window`、フィールドに渡すなら `Phase` か `clamped()` した `Window`**。

> **footgun ③**: 生の `Window` をフィールドに置いてもコンパイルは通ります。症状は「キャッシュヒット率がゼロになる」だけなので気づきにくい — 境界を越える `Window` には `clamped()`、と覚えてください。

## 5. 周期アニメーション — `cycle` / `bounce` / `wave`

一定周期で繰り返す動きは3兄弟で表します。すべて `Phase` を返すので、そのまま easing に流せます。

```rust
t.cycle(2.0)    // sawtooth: 0 → 1 linearly, then snaps back   /| /| /|
t.bounce(2.0)   // triangle: 0 → 1 → 0, linear both ways       /\ /\ /\
t.wave(2.0)     // sine:     0 → 1 → 0, zero slope at the turnarounds
```

- 行って戻る往復運動は `bounce`（`timeline_to_mp4` のドットがこれ）
- 揺らぎ・呼吸・ドリフトのような滑らかな振動は `wave`。`±amp` の振れ幅は `t.wave(period).linear(-amp, amp)`
- 何周目かが必要なら `(t.seconds() / period).floor()` で取れます

## 6. 型付き補間 — `Easing` / `eased` / `Interpolate`

`f32` 以外の量（`Vec2`・`Anchor` …）を補間するときは、カーブを **`Phase` の中で** 当ててから `Interpolate` に渡します。

```rust
use tellur_core::easing::Easing;
use tellur_core::interpolate::Interpolate;

// Ease the progress, then drive a typed lerp with it.
let p = t.phase(1.0, 2.0).eased(Easing::OutCubic);
let pos = start_pos.interpolate(end_pos, p);
let anchor = Anchor::CENTER_LEFT.interpolate(Anchor::CENTER, p);
let tint = INK.interpolate(MUTED, p); // Color: sRGB のまま各チャンネルを直線補間
```

`Easing` はカーブを値として持つ enum（`Linear` / `Smoothstep` / `OutCubic` / `OutQuint` / `InOutQuint` / `InOutExpo` / `InBack` / `OutElastic`、そして自作カーブの `CubicBezier { x1, y1, x2, y2 }`）で、§1 の `ease_*` メソッド群と同じ実装を共有しています。

`Interpolate` の実装は `f32` / `Vec2` / `Anchor` / `Color` にあります。`Color` は sRGB の各チャンネルをそのまま線形補間する素朴な混色で、linear-light に変換してから混ぜる「物理的に正しい」ブレンドではありません — 手書きの `r + (other.r - r) * t` と同じ数値になります。

> **footgun ④**: `eased` は `Phase` に留まるため **overshoot 系カーブ（`InBack` / `OutElastic`、y が単位区間を出る `CubicBezier`）は単位区間に clamp されます**。はみ出しを活かしたいときは `(from, to)` メソッド（`p.ease_out_elastic(from, to)` / `p.ease_bezier(x1, y1, x2, y2, from, to)`）で値域に直接 ease してください。

## 7. 配置クロックの世界 — `Clock` の2軸

`Timeline` / `Sequence` に `.at(..)` で置かれたコンポーネントは、`#[clock]` で `Clock` を受け取ります。`Clock` は **2本の時間軸** を運びます。

```rust
#[component(timeline)]
fn Spinner(#[clock] clock: Clock) -> impl TimelineComponent {
    let local = clock.local();    // 0 at THIS clip's resolved start
    let global = clock.global();  // absolute timeline time — the Event axis
    // ...
}
```

- **`local()`** — 自分のクリップの開始が 0。`Sequence` の並び替えに追従するので、自己アニメーションはこちらで書きます: `clock.local().phase(0.0, 0.4)`
- **`global()`** — タイムライン絶対時刻。`Event` のトリガと同じ軸です
- **`window()`** — 自分のスロットの長さを `Option<Window>`（local 軸上の `[0, 長さ)`）で返します。開いた配置（`.fill()`・素の timeless 配置・ルート）では `None`。ここから先は §3 の語彙がそのまま使えます:

```rust
// Slide in over 0.32s, out over the last 0.24s of this clip's slot.
let alpha = match clock.window() {
    Some(w) => w.envelope(0.32, 0.24).get(),
    None => clock.local().phase(0.0, 0.32).get(),  // open-ended: fade in only
};
```

配置の語彙は3つだけです。

```rust
clip.at(2.0)        // place at 2.0s, play at native length
clip.at(0.0..3.0)   // an explicit window — for a timed clip this is a STRETCH
clip.fill()         // stretch to the container's resolved length (Timeline only)
media.trim(1.0..4.0)   // child-local [1s, 4s) を残し、local 0 へ再基底化
media.trim(-3.0..-0.5) // 負の端点は直接の child end から逆算
media.trim(1.0..)       // open end は正確な child end
```

`trim` は media leaf の metadata ではなく、汎用の component wrapper です。video・audio・cue・trigger・arrangement のすべてを同じ時計で切り詰めます。標準 range は半開区間で、inclusive range は意図的にサポートしません。

### 順序を持つ audio effect

audio のgain automationも同じ wrapper model です。負の数値は直接の child end からの秒数で、正確な終端には `EnvelopePoint::End` を使います。

```rust
use tellur::core::timeline_container::AudioFile;
use tellur::prelude::*;

let voice = AudioFile::builder()
    .path("voice.wav")
    .gain_envelope((0.0, 0.0), (0.35, 1.0))
    .fade_out(0.25)
    .at(2.0);

let tail = AudioFile::builder()
    .path("tail.wav")
    .gain_envelope((-0.5, 1.0), (EnvelopePoint::End, 0.0));
```

builder call はその場で wrapper を作るため、最後の call が最も外側になり、順序が意味を持ちます。

```rust
source.fade_in(1.0).trim(0.5..)
// Trim<GainEnvelope<Source>>: 既存fadeの0.5秒地点から出力が始まる

source.trim(0.5..).fade_in(1.0)
// GainEnvelope<Trim<Source>>: trim後のlocal 0秒から新しいfadeが始まる
```

標準の順序は、source 設定 → temporal/audio effect → 配置です。`.at(..)` 後の component を意図的にさらに wrap することもできますが、その場合は配置が作る先頭区間も外側effectの時計に含まれます。`.fill()` は `Timeline` が自分の尺計算からその child を除外するための構造markerなので、必ず最後・最外側に置きます。つまり `source.fade_out(0.25).fill()` であり、`source.fill().fade_out(0.25)` ではありません。

「長さそのもの」だけが欲しい場面 — `Sequence` に間を置く、トリガーを掛ける台にする、`Timeline` の尺を確定させる — には `TimeBox` を置きます。何も描かず・鳴らさず、指定した `duration` を持つだけのリーフです（空間側 `SizedBox` の時間版）。

```rust
// A 1.5s beat between two clips; nothing is drawn or heard.
Sequence::builder()
    .child(intro)
    .child(TimeBox::builder().duration(1.5).build())
    .child(outro)
    .build()
```

### `Event` — 解決済みの瞬間を木全体で共有する

「このクリップが始まった瞬間に、別のオーバーレイを発火させたい」は `Event` で書きます。

```rust
let cue = Event::named("chorus");

Timeline::builder()
    .child(Sequence::builder()
        .child(intro)
        .child(chorus.trigger_at_start(cue))  // fires when ITS resolved start arrives
        .build())
    .child(Reveal::builder().event(cue).build().fill())
    .build()

// Inside Reveal:
let burst = cue.phase(&clock, 0.0, 0.5).ease_out_cubic(0.0, 1.0);
```

resolve パスがトリガ時刻を表に焼き込み、`Clock` 経由で全コンポーネントから読めます。未発火のイベントは `+∞` 扱い（`phase` は 0 のまま）です。

### `Event::window` — 発火からの区間を窓で持つ

`event.phase(&clock, a, b)` は §1 の `phase` と同じく、写像した瞬間に区間を忘れます。発火を起点にサブイベントをスタッガーしたい・経過秒が欲しいときは、発火時刻を基準にした `Window` を作ります。

```rust
let reveal = cue.window(&clock, 0.0..8.0);  // [発火+0s, 発火+8s) の Window
let ring_in = reveal.sub_secs(0.55..1.05).ease_in_out_expo(0.0, 1.0);
let ink = reveal.elapsed();                 // 発火からの経過秒（窓が閉じたら凍結）
```

返るのは **clamped 済みスナップショット**（§4）です。窓の手前と未発火では `(start, end, start)`、飽和後は `(start, end, end)` で一定 — そのままコンポーネントのフィールドに置けて、アニメーションが終わればキャッシュキーも凍ります。未発火（発火時刻 `+∞`）でも panic せず「開始前」として振る舞います。`elapsed()` が生の `Window` と違って窓の終端で止まる点だけ注意 — 終わりのない蓄積が欲しいなら区間を十分長く取ってください。

> **footgun ⑤**: `.trigger_*` は **外側** に付けます。`x.at(5.0).trigger_at_start(e)` と書くと、トリガは内側の `.at(5.0)` を無視して自分が渡された開始時刻に記録されます。正しくは `x.trigger_at_start(e).at(5.0)`、またはコンテナが位置決めする子に直接付けます。
>
> **footgun ⑥**: `.at(a..b)` の窓は半開区間 `[a, b)` です。隣接するクリップが境界フレームで二重描画されることはありません。

## 8. 決定表

| やりたいこと | 使うもの |
|---|---|
| 区間内の進捗で値を駆動する | `t.phase(a, b).ease_*(from, to)` |
| 窓の中に秒単位でサブイベントを刻む | `t.window(a, b).sub_secs(r).ease_*(..)` |
| 窓が閉じた後も続く動き（回転・ドリフト） | `t.window(a, b).elapsed()` |
| 閉じるまでのカウントダウン | `window.remaining()` |
| 出現・退場のフェード | `window.envelope(fade_in, fade_out)` |
| 区間ゲート（範囲外では描かない） | `t.during(a, b)` / `window.is_inside()` |
| 往復・繰り返し・揺らぎ | `t.cycle(p)` / `t.bounce(p)` / `t.wave(p)` |
| フレームレートの量子化（カクつき表現） | `t.fps(n)` |
| 自作カーブ（CSS の cubic-bezier） | `Easing::CubicBezier` / `p.ease_bezier(..)` |
| `Vec2` / `Anchor` / `Color` などを eased に補間 | `a.interpolate(b, p.eased(Easing::X))` |
| コンポーネントのフィールドに渡す | `Phase`、または `Window::clamped()` |
| クリップの並びに追従する自己アニメ | `clock.local()` / `clock.window()` |
| 別のクリップの解決済み開始に反応する | `Event` + `.trigger_*` + `event.phase(&clock, a, b)` |
| 発火起点の窓でスタッガー・経過秒 | `event.window(&clock, a..b)` |
| 固定長の間・トリガー台・尺の確保 | `TimeBox` |
| end 基準を含む任意componentの切り詰め | `.trim(a..b)` / `.trim(-a..-b)` |
| audio contributionのfade・automate | `.fade_in(d)` / `.fade_out(d)` / `.gain_envelope(..)` |

## 9. 実例 — 2つの世界の合流

`tellur-renderer/examples/timeline_to_mp4.rs` のドットは絶対時刻の世界の最小形です。

```rust
#[component(raster)]
fn BouncingDot(#[builder(into)] t: LocalTime) -> impl RasterComponent {
    let rx = t.bounce(2.5).linear(0.0, 1.0);   // periodic Phase → anchor ratio
    Frame::builder()
        .width(SizeMode::Fill)
        .height(SizeMode::Fixed(60.0))
        .child(
            circle
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(rx, 0.5)),
        )
        .build()
}
```

一方 `tellur-live` の `timeline_showcase` は配置クロックの世界で、`clock.local()` 駆動の自己アニメと `clock.global()` 駆動の全体進行を1画面に共存させています。`demo_scene` は絶対時刻の世界で、`phase` / `window` / `clamped()` のパターンがすべて登場します。**演出の時間設計は絶対時刻で、構成の時間設計は配置クロックで**。レイアウトの「演出は座標で、構造はフローで」と同じ使い分けです。
