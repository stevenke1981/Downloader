# v0.5.1 驗證範圍

本版是 0.5.0 的修正版，只改下列兩處；其餘功能與驗證範圍同 `validation-0.5.0.md`（Windows 安裝包內未附該檔，可於原始碼 `docs/` 取得）。GUI、核心／CLI／host、瀏覽器擴充功能、Windows 安裝包與 macOS app 版本統一為 0.5.1。

## 修正與自動化檢查

- 損壞的 `downloads.json` 若無法移至 `downloads.json.corrupt`，不再被自動儲存覆寫，改存 `downloads.json.recovered`。新增測試以「`.corrupt` 位置有非空資料夾」使搬移失敗，確認原檔不變、`.recovered` 有本次清單；將修正改回舊行為時該測試失敗（mutation 檢查）。
- 「下載所有連結」改以 origin + path 查 Cookie，限定路徑（如 `Path=/members`）的登入 Cookie 會一併送出。以 Node `vm` 載入 `background.js` 並模擬 `chrome.cookies.getAll`（RFC 6265 路徑比對）驗證：新版 3/3 正確，舊版在 `/members/*` 漏帶 Cookie。
- `cargo fmt -p downloader --check`、`cargo clippy --release --workspace --all-targets --locked -- -D warnings`、`cargo build --release --locked`、`cargo test --release --locked` 於 Windows、Linux、macOS CI 執行；通過的測試由 60 項增至 61 項（1 個原生渲染 fixture 標記 ignored）。
- Windows CI 執行安裝／移除煙霧測試。

## 未驗證事項

- 擴充功能未在真實 Chrome／Edge 中載入實測 Cookie 轉送；批次大量連結時逐路徑查 Cookie 的效能未量測。
- `settings.json` 損壞且無法移開時，使用者手動儲存設定仍會覆寫它（只在使用者主動儲存時發生）。
- 其餘沿用 0.5.0 的未驗證事項：GUI 原生滑鼠操作驗收、Unix `ipc.json` 0600 實機檢查、防毒鎖檔情境；Windows 安裝包未簽章，macOS 僅 ad-hoc 簽章、未公證。
