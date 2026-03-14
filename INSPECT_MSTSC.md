# mstsc /l ダイアログ傍受の調査記録

## 目的
`mstsc.exe /l` が表示するモニター情報ダイアログのテキストを、ダイアログを表示せず・音を鳴らさずに取得する。

## 調査結果

### 呼び出しチェーン (確定)
```
mstscax.dll
  → comctl32 内部関数 (エクスポートされていない)
    → Comctl32.dll+0x1349a3 (= TaskDialogIndirect+0x73 の本体内部)
      → Comctl32.dll+0x10bb43 → DialogBoxIndirectParamW (user32.dll)
        → CreateWindowExW("DirectUIHWND") + 子コントロール群
```

### フック対象として試したAPI一覧

| API | DLL | BPヒット | インラインフック |
|-----|-----|---------|--------------|
| MessageBoxW | user32 | 0 | 呼ばれず |
| MessageBoxExW | user32 | 0 | 呼ばれず |
| MessageBoxIndirectW | user32 | 0 | 呼ばれず |
| MessageBoxA | user32 | - | 呼ばれず |
| MessageBoxExA | user32 | - | 呼ばれず |
| DialogBoxParamW | user32 | 0 | 呼ばれず |
| **DialogBoxIndirectParamW** | user32 | **1** | - |
| TaskDialog | comctl32 | 0 | - |
| TaskDialogIndirect | comctl32 | 0 | エントリポイント経由せず呼ばれず |
| CreateWindowExW | user32 | 28 | - |

### 重要な発見

1. **mstscax.dll は comctl32 の内部関数を直接呼んでいる**
   - TaskDialog/TaskDialogIndirect のエクスポートされたエントリポイントは通らない
   - comctl32 内部で TaskDialogIndirect の本体コード (+0x73) に到達するが、先頭 (+0x0) を通らない

2. **DialogBoxIndirectParamW は確実にヒットする**
   - 呼び出し元: Comctl32.dll+0x10bb43
   - パラメータ: RCX=comctl32 base, RDX=dialog template, R8=0 (parent), R9=dialog proc

3. **ダイアログは DirectUI ベース**
   - TaskDialog が内部的に使う DirectUIHWND + CtrlNotifySink + ScrollBar + SysLink + Button
   - テキストは Static コントロールではなく DirectUI 要素として描画される

4. **子プロセスは無い** — mstsc.exe は単一プロセス

5. **モニターテキストは RSP+0x78 にワイド文字列ポインタとして存在する**
   - DialogBoxIndirectParamW ヒット時の RSP+0x78 がモニターテキスト文字列への直接ポインタ
   - TASKDIALOGCONFIG 構造体経由ではなく、ヒープ上のワイド文字列を直接指している
   - comctl32 の TaskDialogIndirect フレーム内で使われるローカルデータ

### スタック上のアドレス (DialogBoxIndirectParamW ヒット時)

```
RSP+0x00: Comctl32.dll+0x10bb43  (DialogBoxIndirectParamW の呼び出し元)
RSP+0x40: Comctl32.dll+0x13413b
RSP+0x78: → ヒープ上のモニターテキスト (UTF-16LE ワイド文字列)  ★ここ
RSP+0x90: Comctl32.dll+0x1349a3  (= TaskDialogIndirect+0x73)
```

comctl32 のアドレス:
- TaskDialog: comctl32+0x134860
- TaskDialogIndirect: comctl32+0x134930
- 0x1349a3 = TaskDialogIndirect+0x73

### 取得されるテキストの形式

```
0: 1920 x 1200; (3840, 241, 5759, 1440)
1: 3840 x 2160; (0, 0, 3839, 2159)
2: 2560 x 1440; (-2560, 0, -1, 1439)
3: 1920 x 1080; (1736, -1080, 3655, -1)
```

各行: `ID: WxH; (left, top, right, bottom)`
- 座標は inclusive bounds (right-left+1 = width)
- PRIMARY 表記は無い（EnumDisplayMonitors で別途取得）

## 実装済みの捕獲方式

### 方式: DialogBoxIndirectParamW BP + RSP+0x78 読み取り (採用・実装済み)

`capture_mstsc_bp()` として実装。

1. `mstsc.exe /l` を `DEBUG_PROCESS` で起動
2. DialogBoxIndirectParamW に int3 (0xCC) ブレークポイントを設置
3. BP ヒット時に RSP+0x78 のポインタを辿りワイド文字列を読み取り
4. テキスト取得後、即座にプロセスを終了（ダイアログ表示なし・音なし）
5. テキストをパースしてモニター情報を返す

RSP+0x78 が失敗した場合は RSP+0x00〜0x1F8 の全スロットをスキャンするフォールバック付き。

### フォールバック: EnumDisplayMonitors ID
BP capture が失敗した場合は EnumDisplayMonitors の ID をそのまま使用。
再起動後にモニター ID がずれる可能性があるため、UI のタイトルバーに "ID fallback" バッジを表示。
