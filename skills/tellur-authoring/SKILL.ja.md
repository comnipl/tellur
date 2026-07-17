# Tellur らしいコードを書く

> 本書は [SKILL.md](./SKILL.md)（英語・正本）の日本語版です。エージェントが読み込むのは
> frontmatter を持つ英語版のみで、このファイルは人間向けの翻訳です。内容が食い違う場合は
> 英語版を優先してください。

tellur は Rust 製の宣言的な動画・モーショングラフィックスエンジンです。このスキルは
「動くコード」ではなく「**Tellur らしいコード**」を書くための規範を定めます。
tellur のコードには明確な書き味があり、それを外すとレビューで書き直しになります。

## 0. 必読ドキュメント

コードを書き始める前に、tellur リポジトリの以下 2 つのチュートリアルを **必ず通読**
してください（このスキルはチュートリアルの置き換えではなく、その上に載る規範です）:

- `docs/layout-tutorial.md` — レイアウトシステム（キャンバス世界とフロー世界）
- `docs/time-tutorial.md` — 時間システム（絶対時刻の世界と配置クロックの世界）

このスキルディレクトリが tellur リポジトリの外にある場合は、tellur のチェックアウト
（または https://github.com/comnipl/tellur の docs/）から取得して読んでください。
日本語訳が `docs/*.ja.md` にありますが、正本は英語版です。

チュートリアルに載っている機能は「知っていれば使う」ではなく「**使うことが前提**」
です。手書きの数式や座標計算を書く前に、対応する語彙が無いか両方の決定表
（layout §8 / time §8）を確認してください。

## 1. メンタルモデル — React としての tellur

tellur は React に近い宣言的モデルです。1 フレームの絵は「時間の純関数」であり、
コンポーネントツリーを毎フレーム評価した結果です。

- **状態の変異やイベントループは書かない。** アニメーションは `Time → Phase/Window →
  値` の変換で表現する
- **コンポーネントのフィールドはレンダーキャッシュのキー**。同じフィールド値なら同じ
  絵、が成立するように書く（§5）
- `Fragment` = `<>...</>`、`Fragment::empty()` = `null`、`Positioned` = 位置指定、
  という対応で「なし・1 つ・複数・位置つき」がすべてコンポーネントで表現できる

## 2. 鉄則① — すべてはコンポーネント

**`#[component(...)]` の付いていない関数が現れることはかなりまれです。**
tellur 流は、すべてをコンポーネント化し、Builder パターンで構築したツリーを返すこと:

```rust
#[component(vector)]
fn TitleCard(#[builder(into)] source: String, alpha: f32) -> impl VectorComponent {
    Text::builder()
        .font(SERIF.clone())
        .size(72.0)
        .fill(INK.multiply_alpha(alpha))
        .span(MathSpan::new(source))
        .anchored(Anchor::CENTER)
        .snap_to(CENTER)
}
```

- 関数形式コンポーネントは **PascalCase**。`#[component(vector)]` /
  `#[component(raster)]` / `#[component(timeline)]` の 3 種
- 完成した builder は `.build()` なしでそのまま親の `.child(...)` に渡せる
  （`From<TBuilder>` が生成される）。**子として渡すときは `.build()` を書かない**のが
  通例。関数の返り値として返すときだけ `.build()` する
- builder の便利機能を活用する: `#[builder(into)]`（`&str` → `String` など）、
  `maybe_*` セッター（`Option` をそのまま渡す。`if let` で分岐して builder を
  組み替えるより優先）、`#[children(each = child)]`（`Vec` の子を `.child()` の
  連打で積む）
- `#[clock] clock: Clock` は timeline コンポーネント専用。`#[available]` は
  親が割り当てたサイズを受け取る layout 用の穴

**素の関数が許されるのは**、レンダーツリーを返さない純データの下ごしらえ
（`Vec<PathCommand>` の生成、`Keyable` な state 構造体の組み立てなど）に
ほぼ限られます。素の関数を書きたくなったら、まず「これは tellur の既存語彙
（shape・layout・time コンビネータ）に無いのか」を疑ってください。過去の実例では、
手書きヘルパーの大半（自前 easing、破線分割、スタッガー計算、色 lerp）が
**tellur 側に語彙が増えたことで消滅**しています。手書きヘルパーは多くの場合
「tellur のギャップ」のシグナルであり、動画側で抱えるものではありません。

## 3. 鉄則② — マジックナンバー・マジック数式は語彙で消す

手書きの数式が出てきたら、ほぼ確実に対応する tellur の語彙があります。

### 時間の変換表

| 手書きしがちな式 | Tellur らしい書き方 |
|---|---|
| `((x - a) / (b - a)).clamp(0.0, 1.0)` | `t.phase(a, b)` |
| 自前イージング多項式・cubic-bezier ソルバ | `phase.ease_out_cubic(from, to)` など / `ease_bezier(x1, y1, x2, y2, from, to)` / `Easing::CubicBezier` |
| `a + (b - a) * p` | `p.linear(a, b)` / `a.interpolate(b, p)`（`f32`/`Vec2`/`Anchor`/`Color`） |
| フェードイン式 × フェードアウト式の手組み | `window.envelope(rise, fall)` |
| `progress - i as f32 * stagger` のループ | `window.sub_secs(range)` でスタッガー |
| `(t % period) / period` / 三角波 / `sin` | `t.cycle(p)` / `t.bounce(p)` / `t.wave(p)` |
| 「終わってから n 秒」の手計算 | `window.elapsed()` / `window.after()` |
| カウントダウン | `window.remaining()` |
| `if (a..b).contains(&t)` ガード | `t.during(a, b)` — ただし §6 の通り不可視ガード自体が大抵不要 |

- 逆方向のアニメーションは式をいじらず **`(from, to)` を入れ替える**:
  `phase.ease_in_back(1.0, 0.0)`
- 「出現しつつ回転し続ける」のような複合は `Window` 1 つに束ねる:
  `w.phase()` と `w.elapsed()` を同じ宣言済み区間から取る
- 複数条件の可視性は **`[0,1]` 因子の積** で合成する:
  `let alpha = after(cue_in) * (1.0 - entering(cue_out, 0.0, 0.45));`

### 空間の変換表

| 手書きしがちな計算 | Tellur らしい書き方 |
|---|---|
| `Vec2(canvas.0 * 0.5 - w * 0.5, ...)` の中央寄せ | 絶対配置なら `.anchored(Anchor::CENTER).snap_to(point)` / 解決済み box 内なら `.snap_to(Anchor::CENTER)` |
| 兄弟の座標を等差数列で計算 | `Flex` + `.spacing(n)` |
| 子を手動計測して装飾・overlay のサイズを合わせる | `Stack::builder().under(...).base(child).over(...)` |
| 右寄せ・下寄せの座標逆算 | `Anchor::BOTTOM_RIGHT` 系 / `Flexible::spacer(1.0)` / `MainAlign::End` |
| 余白ぶんのオフセット加算 | `Padding` + `EdgeInsets` |
| 中心回転のための平行移動サンドイッチ | `.transform_around(Anchor::CENTER, t)` |
| 「箱の中を子が滑る」座標補間 | `.anchored(Anchor::CENTER).snap_to(Anchor::new(rx, 0.5))` で target anchor を時間駆動 |
| はみ出しを消すための座標クランプ | `Clip` / `DecoratedBox`（raster） |

**演出は座標で、構造はフローで。** モーショングラフィックスの演出配置
（キャンバス世界: `Layer` + `place_at` / `anchored().snap_to()`）では絶対座標
リテラルは演出判断そのものなので許容されます。字幕バー・HUD・表のような「UI 的」
構造をキャンバス世界の座標計算で組み始めたら、それはフロー世界
（`Frame`/`Flex`/`Stack`/`Padding`）の仕事です。

## 4. 鉄則③ — `const` の増殖は設計ミスのシグナル

マジックナンバー回避のつもりで `const` 定義が並び始めたら、**それは名前の問題では
なく構造の問題**です。グローバルな `const` は少ないほど良い。増えてきたら、
本来レイアウト用・時間管理用のコンポーネントがやるべき仕事を、名前付き数値で
無理矢理「配置」して実現していないか疑ってください。

`const` 化を考える前のチェック:

1. **その数値は別の数値から導出されていないか？** `const TITLE_X: f32 = CANVAS_W *
   0.5 - 120.0;` のような算術関係は、`Anchor` の相対指定や
   `Frame`/`Flex`/`Stack`/`Padding` で構造として表現する。導出式を const に
   閉じ込めるのは隠蔽であって解決ではない
2. **時刻表になっていないか？** `const SCENE2_START: f64 = 12.3;` のような絶対時刻の
   カスケードは、`Sequence` の並び・`TimeBox` の間・`Event` トリガで「配置から時刻が
   導かれる」形にする。台本を差し替えても壊れないのが正しい構造
3. **2 箇所以上で同じ数値を使っているか？** 1 箇所でしか使わない演出リテラル
   （`Vec2(320.0, 750.0)` に置く、0.45 秒でフェードする等）は、コンポーネント内の
   ローカル変数か直書きで十分。すべての数値に名前を付ける必要はない

**正当な `const`（デザイントークン）**: パレット色（`INK`・`BLUE`）、キャンバス
サイズ、図の中心と半径のような複数シーンで共有される少数の幾何トークン、意味のある
共有時間（複数箇所で揃えるフェード時間）。これらは `style.rs` に少数だけ置きます。
非自明な値（音量ゲイン、余裕をもたせた窓長など）には **由来を説明するコメント**
を必ず添えてください。

```rust
// -6.63 dB over the 91.816s preview BGM window: -15.37 LUFS -> -22.0 LUFS.
const BGM_GAIN: f32 = 0.466122427;
```

## 5. 鉄則④ — キャッシュキーを意識した境界設計

コンポーネントのフィールドはレンダーキャッシュのキーです。時間駆動の値を
コンポーネント境界を越えて渡すときの型選択が、キャッシュ効率を決めます。

- その場で値まで潰すなら `phase`、サブイベントや経過秒が要るなら `window`
- **フィールドに渡すのは `Phase` か、`clamped()` 済みの `Window`**。生の `Window` は
  カーソルが毎フレーム動き、キャッシュが全滅する（コンパイルは通るので気づきにくい）
- `Event` 起点なら `event.window(&clock, a..b)` が clamped 済みスナップショットを
  返すので、そのままフィールドに置ける
- state 構造体は `#[derive(Clone, Keyable)]`（`Copy` も可能なら付ける）。`Keyable` は
  f32 をビットパターンで `Eq`/`Hash` 化する tellur の derive

## 6. 描画とアニメーションのイディオム

- **可視性ガードを書かない。** 不可視のもの（alpha 0、空文字、opacity 0）は自分で
  空を描く。`if alpha > 0.0 { ... } else { Fragment::empty() }` は不要。
  `Fragment::empty()` は「構造として何も無い」を表すときだけ使う
- **alpha は因子の積で合成**し、`Color::multiply_alpha(alpha)` /
  `Color::with_alpha(base * alpha)` で色に適用する
- 書き文字アニメは `Text` / shape の `.write_elapsed(secs)` / `.write_on(phase)`
  （+ `.stroke_end(Phase::ONE)`）。経過秒は `Event::window` の `.elapsed()` から取る。
  ペーシングはデフォルトでグリフごとの等時間スロット（stagger 付き）。リズムは
  `.per_path_secs(secs)` / `.lag_ratio(r)` で調整し、画数の多い字ほど時間をかけたい
  ときは `.stroke_speed(u)`（`Write` では `.by_length()`）でペン速度一定に切り替える
- overshoot 系（`ease_in_back` / `ease_out_elastic` / 単位区間を出る bezier）は
  `(from, to)` メソッドで値域に直接 ease する。`eased(Easing::X)` は `Phase` に
  留まるため clamp される。負方向のはみ出しが困る文脈では `.max(0.0)` を添える
- 位置とサイズが同時に動く要素は、`Phase` 1 つから `interpolate` で両方を導く
  （`MovingTitleMath` パターン: `start_*.interpolate(end_*, progress)`）

## 7. 動画プロジェクトの標準構造

実績のある構成（shorts_sqrt2_plus_sqrt3 型）:

```
src/
  lib.rs        -- build() -> Timeline; export_timeline!; トップレベル合成のみ
  script.rs     -- 台本 (音声・字幕クリップを並べた Sequence) + Cues 構造体 (Event の束)
  <canvas>/
    mod.rs      -- #[component(timeline)] ルート: cues + #[clock] から全シーンの
                   state を導出し、シーンを Layer に重ねる
    style.rs    -- デザイントークン (パレット・共有幾何) を少数だけ
    widgets.rs  -- 小さな再利用コンポーネント (数式テキスト、図形ラッパ)
    scenes/     -- 1 シーン 1 ファイル。SceneState 構造体 + #[component(vector)]
```

この構成の要点:

1. **キュー駆動**: 台本（音声クリップ等を並べた `Sequence`）が `Event::named` の束
   （`Cues` 構造体、`Copy + Keyable`）を `.trigger_at_start(cue)` /
   `.trigger_at(offset, cue)` で発火し、絵はすべて Event に反応する。
   台本の尺が変わっても絵のコードは 1 行も変わらないのが正しい状態
2. **時間はルートに集約**: `#[clock]` を持つルートコンポーネントが、すべての
   時間駆動スカラー（alpha・progress・`Window`）を **1 箇所で** cues から導出し、
   `Keyable` な state 構造体に束ねてシーンに渡す。シーン以下は時間を知らない
   純粋な state の関数になり、キャッシュも効きテストもしやすい
3. **シーンの重なりは因子の積**: `after(prev_cue) * (1.0 - entering(next_cue, ..))`
   でシーン間クロスフェードを表現し、全シーンを常に `Layer` に置いておく
4. ルートで繰り返すヘルパーはクロージャで束ねる:
   `let after = |cue: Event| cue.phase(&clock, 0.0, 0.45).ease_out_cubic(0.0, 1.0);`

時間配置の語彙は 4 つだけ: `.at(secs)`（絶対配置）、`.at(a..b)`（窓 = timed clip
にはストレッチ）、`.fill()`（コンテナ長に合わせる）、`.trim(a..b)`（任意の
component を wrap して再基底化）。負の trim 端点は直接の child end から逆算し、open end は正確な終端を意味する。「間」や「尺の確保」は `TimeBox`。

audio gain effect も順序を持つ wrapper: `.gain_envelope((time, gain), (time,
gain))`、`.fade_in(seconds)`、`.fade_out(seconds)`。builder call はその場で wrap
するため、`x.fade_in(1.0).trim(0.5..)` は内側fadeの0.5秒地点から始まり、
`x.trim(0.5..).fade_in(1.0)` はtrim後のlocal zeroから新しいfadeを始める。
source 設定 → effect → 配置の順を推奨する。`.fill()` は構造markerなので必ず最後・最外側に置き、`source.fade_out(0.25).fill()` と書く（逆順は禁止）。

## 8. footgun 一覧（チュートリアルより・要暗記）

1. `SizeMode::Fill` は親が `UNBOUNDED` だと 0 に潰れる（キャンバス世界は子を無限
   制約で測る）
2. `Flexible` / `.grow()` は `Flex` の**直下の子**でだけ効く
3. grow を持つ子がいると `MainAlign` の `Center`/`End`/`Space*` は `Start` に退化する
4. main 軸が無限制約のとき grow は不活性
5. `phase` / `window` は `end > start` を要求し、破ると panic
6. `Window::elapsed()` / `after()` は窓が閉じても止まらない（それが存在理由）。
   止めたければ `remaining()` か `clamped()`。ただし `Event::window` 由来の
   `elapsed()` は窓の終端で凍結する
7. 生の `Window` をフィールドに置くとキャッシュが全滅する（§5）
8. `eased(Easing::X)` は overshoot カーブを clamp する（§6）
9. `.trigger_*` は `.at(..)` の**外側**（前）に付ける:
   `x.trigger_at_start(e).at(5.0)`
10. `.at(a..b)` は半開区間 `[a, b)`。境界フレームの二重描画は起きない
11. `Clip` は vector 専用（raster で欲しければ切ってから `.rasterize()`）。
    `.transform()` / `.transform_around()` も vector 専用。`.opacity()` は両方にある
12. 再エクスポート（`some_crate::tellur`）越しに `#[component]` を使う場合、
    マクロのパス解決のため動画クレート側にも同一版の `tellur` 直接依存を併記する
13. temporal builder call は順序付きwrapper。`.trim()` と audio effect の順を入れ替えると、
    effect が見るlocal clockも意図的に変わる
14. `.fill()` は親 `Timeline` が尺計算から除外できるよう、必ず最外側のtemporal verbにする

## 9. 参照実装

書き方に迷ったら、この優先順で実例を見てください:

- `tellur-renderer/examples/timeline_to_mp4.rs` — 絶対時刻の世界の最小形
  （`BouncingDot`: サイズ済み `Frame` 内で `Positioned` の target anchor を
  時間駆動する教科書パターン）
- `tellur-live` のデモシーン — キャンバス世界の演出と `phase`/`window`/`clamped()`
  の全パターン、および配置クロックの世界（`timeline_showcase`）
- youtube リポジトリの `movies/202606/shorts_sqrt2_plus_sqrt3` — キュー駆動
  アーキテクチャ（§7）を持つ完全な動画。ただし §4 の `const` 規範の確立前の
  コードなので、named-const の多さはそのまま真似しないこと

## 10. セルフレビュー チェックリスト

コードを提出する前に確認する:

- [ ] `#[component]` の付いていない関数は、純データの下ごしらえだけか。
      レンダーツリーや進捗計算を素の関数でやっていないか
- [ ] 手書きの clamp / lerp / easing / スタッガー式が残っていないか
      （§3 の変換表で置き換えたか）
- [ ] 座標の算術（`* 0.5`、`- width / 2.0`、等差数列）で相対関係を表現して
      いないか。`Anchor` / `Frame` / `Flex` / `Stack` / `Padding` で書けないか
- [ ] グローバル `const` は真のデザイントークンだけか。導出値・時刻表・
      1 箇所でしか使わない値を const 化していないか（§4）
- [ ] 非自明な数値リテラルに由来を説明するコメントがあるか
- [ ] コンポーネント境界を越える `Window` はすべて `clamped()`（または
      `event.window`）か
- [ ] state 構造体に `Keyable` が付いているか
- [ ] 可視性ガード（`if alpha > 0.0`）を書いていないか
- [ ] alpha を因子の積で合成しているか。逆方向アニメは `(from, to)` の
      入れ替えで書いているか
- [ ] 台本の尺・順序を変えても絵のコードが壊れない構造か（Event 駆動か）
