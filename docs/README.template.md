*: # RDP Launcher
*:
en: **A GUI launcher for multi-monitor RDP connections that survive monitor ID changes across reboots.**
ja: **再起動でモニターIDがずれても、意図通りのマルチモニターRDP接続ができるGUIランチャー。**
*:
en: ## What it solves
ja: ## 何を解決するか
*:
en: Monitor IDs returned by `mstsc /l` (0, 1, 2...) can change after reboots or dock reconnections.
ja: `mstsc /l` が返すモニターID (0, 1, 2...) は再起動やドック着脱で変わる。
en: The `selectedmonitors:s:0,1` field in `.rdp` files is ID-based, so shifted IDs cause connections to the wrong monitors.
ja: `.rdp` ファイルの `selectedmonitors:s:0,1` はIDベースなので、ずれると意図しないモニターに接続してしまう。
*:
en: **Key insight**: Monitor IDs are unstable, but **physical screen coordinates and resolutions are stable**.
ja: **核心**: モニターIDは不安定だが、**画面の物理座標と解像度は不変**。
en: This tool identifies monitors by coordinates and automatically resolves the correct IDs at connection time.
ja: 本ツールは座標ベースでモニターを識別し、接続時に正しいIDへ自動変換する。
*:
en: ## Features
ja: ## 機能
*:
en: - **Multi-host management**: Register hosts by .rdp file, view as card-based UI
ja: - **複数ホスト管理**: .rdp ファイル単位でホストを登録、カード型UIで一覧
en: - **Display profiles**: Define named profiles like "Left 2 screens", "Right 2 screens", "All screens"
ja: - **ディスプレイプロファイル**: 「左2画面」「右2画面」「全画面」など名前付きで定義
en: - **Host x Profile**: Set a default profile per host, switch at connection time
ja: - **ホスト × プロファイル**: ホストごとにデフォルトプロファイルを設定、接続時に切替も可
en: - **Connection guard**: Confirmation dialog when reconnecting to an already-connected host
ja: - **接続ガード**: 既に接続中のホストに再接続しようとすると確認ダイアログを表示
en: - **Preserves original .rdp**: Only rewrites monitor settings in a temp copy; all other settings are kept
ja: - **元の.rdpを温存**: モニター設定だけ一時コピーに書き換えてmstscに渡す。他の設定はそのまま
en: - **Drag & drop**: Drop .rdp files onto the window to add hosts
ja: - **ドラッグ&ドロップ**: .rdp ファイルをウィンドウにドロップしてホスト追加
en: - **Contiguity check**: Prevents creating profiles with non-adjacent monitors
ja: - **飛び地防止**: 隣接しないモニターの組み合わせでプロファイルを作れないようバリデーション
en: - **Zero dependencies**: Single binary (WebView2 is built into Windows 10/11)
ja: - **ゼロ依存**: シングルバイナリ (WebView2はWin10/11標準搭載)
en: - **Bilingual UI**: Japanese and English, switchable in settings
ja: - **日英対応**: 設定から日本語／英語を切り替え可能
*:
en: ## Usage
ja: ## 使い方
*:
en: ### 1. Initial setup
ja: ### 1. 初回セットアップ
*:
en: 1. Launch the app - connected monitors are auto-detected
ja: 1. アプリを起動 → 接続中のモニターが自動検出される
en: 2. Settings (⚙) → "Profiles" tab → Create a profile
ja: 2. 設定 (⚙) → 「プロファイル」タブ → プロファイルを作成
en:    - Click monitors to select/deselect
ja:    - モニターをクリックで選択/解除
en:    - Double-click to set as Primary
ja:    - ダブルクリックでPrimary指定
en: 3. Add a host
ja: 3. ホストを追加
en:    - "+ Add Host" → specify an .rdp file
ja:    - 「+ ホストを追加」→ .rdpファイルを指定
en:    - Or drag & drop an .rdp file onto the window
ja:    - または .rdp ファイルをウィンドウにドラッグ&ドロップ
en:    - Select the default profile
ja:    - デフォルトプロファイルを選択
*:
en: ### 2. Daily use
ja: ### 2. 日常の使い方
*:
en: - Click the "Connect" button on a host card
ja: - メイン画面でホストカードの「接続」ボタンをクリック
en: - Double-click a card to connect
ja: - カードをダブルクリックでも接続可能
en: - Switch profiles via the dropdown on each card
ja: - 別のプロファイルで接続したい場合はドロップダウンで切替
en: - Drag the grip (☰) to reorder cards (order is saved)
ja: - カードはグリップ (☰) をドラッグして並べ替え可能（順序は保存される）
en: - Works seamlessly after reboots (IDs are resolved each time)
ja: - 再起動後もそのまま動作 (IDは毎回自動解決)
*:
en: ### 3. When your monitor layout changes
ja: ### 3. モニター構成が変わったら
*:
en: If you change the physical display arrangement (Windows Settings > Display > Arrangement):
ja: ディスプレイの物理配置 (Windows設定 > ディスプレイ > 配置) を変更した場合:
en: 1. Settings → Monitors tab → "Auto Detect" to re-detect
ja: 1. 設定 → モニタータブ → 「自動検出」で再検出
en: 2. Review/adjust your profiles
ja: 2. プロファイルの確認/調整
*:
en: ## Configuration
ja: ## 設定ファイル
*:
en: Saved at `%APPDATA%/rdp-launcher/config.json`. Can be edited manually:
ja: `%APPDATA%/rdp-launcher/config.json` に保存。手動編集も可能:
*:
*: ```json
*: {
en:   "language": "en",
ja:   "language": "ja",
*:   "monitors": {
*:     "mon-0": { "name": "left 1920x1080", "left": -1920, "top": 0, "width": 1920, "height": 1080 },
*:     "mon-1": { "name": "center 2560x1440", "left": 0, "top": 0, "width": 2560, "height": 1440 }
*:   },
*:   "profiles": {
*:     "left-two": {
en:       "name": "Left 2 screens",
ja:       "name": "左2画面",
*:       "monitor_ids": ["mon-0", "mon-1"],
*:       "primary": "mon-1"
*:     }
*:   },
*:   "hosts": [
*:     {
*:       "id": "dev",
en:       "name": "Dev Server",
ja:       "name": "開発サーバー",
*:       "rdp_file": "C:\\Users\\me\\dev.rdp",
*:       "default_profile": "left-two",
*:       "color": "#5b9aff"
*:     }
*:   ]
*: }
*: ```
*:
en: ## Troubleshooting
ja: ## トラブルシューティング
*:
en: **Q: Monitors are not detected**
ja: **Q: モニターが検出されない**
en: → Run `mstsc /l` manually in a command prompt and check the output. If a dialog appears, it's working.
ja: → `mstsc /l` をコマンドプロンプトで手動実行して出力を確認。ダイアログが表示されればOK。
*:
en: **Q: "Profile not found" error when connecting**
ja: **Q: 接続時に「Profile not found」エラー**
en: → The host's default profile was deleted. Reassign it in settings.
ja: → ホストのデフォルトプロファイルが削除されている。設定で再指定。
*:
en: **Q: Connection works but displays on the wrong monitors**
ja: **Q: 接続はできるが意図しないモニターに表示される**
en: → Display arrangement may have changed. Settings → Monitors → Auto Detect to update.
ja: → ディスプレイ配置が変わった可能性。設定 → モニター → 自動検出 で更新。
*:
*: ---
*:
*: MIT License
