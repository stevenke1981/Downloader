# v0.4.0 驗證紀錄

來源基線：`9d5788ccd137652c9ddc89aa59718348b122b0e2`（v0.3.0）。

## 已完成

- 獨立程式碼審查：最後一輪未發現 P1／P2 問題。
- Windows 5900X：`cargo test --release --locked -j 2`，38 項測試通過。
- `cargo clippy --release --locked --workspace --all-targets -j 2` 通過；核心與既有 fixture 仍有樣式警告，GUI 正式程式碼無警告。
- `cargo fmt -p downloader --check` 與 `git diff --check` 通過。
- 安裝程式建置／煙霧測試腳本的 PowerShell 語法檢查通過。自製 HTTP 安裝測試 fixture 已在本機驗證內容一致。
- 新增路徑測試涵蓋未完成狀態、缺少／移動檔案、目錄、Unicode、空格與 shell 特殊字元。
- 無畫面 UI 回歸測試使用獨立暫存資料及本機 HTTP fixture，涵蓋反覆切換 30 次、背景下載完成、單一任務身分、退出確認鍵盤操作與系統匣不可用時還原。

## 尚未完成的原生桌面驗收

本次執行環境未提供可用的原生桌面操作工具，未繞過工具限制。無畫面測試不等於實際桌面驗收；目前不能宣稱以下操作已經人工或原生 GUI 自動化驗證：

- Windows 檔案總管實際開啟並選取完成檔案。
- 系統匣左鍵、右鍵選單、Explorer 重啟後恢復，以及前景焦點行為。
- 精簡視窗的置頂、關閉與實際滑鼠操作。

CI 的 Windows 截圖只證明實際視窗渲染，不能取代以上互動測試。下載、編譯及安裝煙霧測試的最終 CI 狀態、附件 SHA256 與來源 SHA 以 GitHub Release 說明及附件為準。

在完成以上互動驗收前，v0.4.0 應保留為預覽版本。Windows 系統匣為本版新增功能；其他平台提供精簡視窗與原有系統最小化。
