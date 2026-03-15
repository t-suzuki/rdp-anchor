# RDP Anchor

**再起動でモニターIDがずれても、意図通りのマルチモニターRDP接続ができるGUIランチャー。**

> **利用ガイド / User Guide**
> - [日本語](docs/README.ja_JP.md)
> - [English](docs/README.en_US.md)

---

## 開発者向け情報

### 前提条件

- **Rust**: https://rustup.rs/ からインストール
- **Tauri CLI**: `cargo install tauri-cli` (v2)
- **just**: `cargo install just` — タスクランナー
- **Windows 10 1903以降** (mstsc /l のサポートとWebView2)

### タスク一覧

```bash
just dev              # 開発モード (ホットリロード)
just check            # コンパイルチェック
just build            # リリースビルド + ZIP生成
just release 0.1.0    # バージョン更新 + コミット + タグ + ビルド
just release-build    # バージョン変更なしでリリースビルド
just gen-readme       # docs/README.template.md からバイリンガルREADME生成
just icon path.png    # アイコン生成 (1024x1024以上のPNG)
```

### 成果物

| ファイル | パス |
|---|---|
| 実行ファイル | `target/release/rdp-anchor.exe` |
| MSI (英語) | `target/release/bundle/msi/RDP Anchor_<ver>_x64_en-US.msi` |
| MSI (日本語) | `target/release/bundle/msi/RDP Anchor_<ver>_x64_ja-JP.msi` |
| ZIP | `target/release/bundle/zip/RDP-Anchor_<ver>_x64.zip` |

### リリース手順

`just release <ver>` は以下を自動実行:
1. `Cargo.toml`, `tauri.conf.json`, `dist/index.html`, `scripts/package-zip.ps1`, `README.md` のバージョン更新
2. コミット + タグ (`v<ver>`)
3. リリースビルド + ZIP/MSI生成

### アーキテクチャ

```
┌─────────────────────────────────────────────────┐
│  Frontend (HTML/CSS/JS in WebView2)             │
│  ・ホストカード一覧                               │
│  ・モニター視覚プレビュー                          │
│  ・プロファイル選択                                │
│  ・設定画面 (モニター / プロファイル管理)            │
└──────────────────┬──────────────────────────────┘
                   │ Tauri invoke (IPC)
┌──────────────────▼──────────────────────────────┐
│  Backend (Rust)                                  │
│                                                  │
│  monitor.rs  mstsc /l パース + Win32 API         │
│              座標ベースでID逆引き                   │
│                                                  │
│  rdp.rs      .rdp ファイル読み書き                 │
│              selectedmonitors だけ動的書き換え      │
│                                                  │
│  session.rs  EnumWindows で接続中セッション検出      │
│                                                  │
│  config.rs   JSON 設定の永続化                     │
│              %APPDATA%/rdp-anchor/config.json     │
└─────────────────────────────────────────────────┘
```

フロントエンドはビルドツールなしの素の HTML/CSS/JS (dist/index.html 単体)。

### 接続フロー

```
[接続ボタン]
    │
    ▼
mstsc /l を実行 → 現在のID⇔座標マップ取得
    │
    ▼
プロファイルの各モニター定義を座標で照合 → 現在のmstsc IDを特定
    │
    ▼
元の .rdp をコピー → selectedmonitors だけ書き換え
    │
    ▼
既に接続中？ → Yes → 確認ダイアログ → No → 中止
    │                      │
    ▼                      ▼
mstsc.exe <temp>.rdp で起動
```

### モニターID検出の優先順位

1. **mstsc /l** (最も信頼性が高い): ダイアログを自動キャプチャ → パース → 自動クローズ
2. **Win32 EnumDisplayMonitors** (フォールバック): 列挙順がmstscのIDと一致する前提

### 座標マッチングの仕組み

1. **完全一致**: left, top, width, height が全て一致 → 確定
2. **解像度一致 + 最近傍**: 同解像度のモニターが複数ある場合、座標が最も近いものを選択
3. **不一致**: エラーで停止し再検出を促す

### .rdp の書き換え方針

- **元ファイルは変更しない**: `<元ファイル名>_launch.rdp` という一時コピーを作成
- 書き換える行: `selectedmonitors:s:`, `use multimon:i:`, `screen mode id:i:`
- その他の設定 (認証、帯域、リダイレクトなど) は全てそのまま保持

### セッション検出

- `EnumWindows` で `TscShellContainerClass` クラスのウィンドウを列挙
- ウィンドウタイトル (例: "myhost - Remote Desktop Connection") からホスト名を抽出
- ホスト名の部分一致で接続状態を判定

## ライセンス

本ソフトウェアは現在無料で利用可能です。著作権は作者に帰属します。
