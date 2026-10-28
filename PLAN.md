# FDM 功能對齊計畫

來源：[維基百科 Free Download Manager](https://zh.wikipedia.org/zh-tw/Free_Download_Manager)，2026-10-28 讀取，頁面 oldid=94816096。以功能描述為驗收輸入，保留 Downloader 現有 Rust/Slint 架構與登入頁拒收行為，不複製 FDM 程式碼或品牌。

## 分工與順序

基準：`6e5cb2bbdd3933b7a613505115defebe0212b318`，main 工作區乾淨。

原生使用者設定協調者 `/root/luna_coordinator`（gpt-6-luna/medium）已選擇三項工作皆用 gpt-6.1-sol/xhigh；不經 OpenRouter。各作者使用獨立 Git worktree，根代理整合並完成使用者已授權的 commit/push。

| 任務 | owner | 寫入範圍 | 完成條件 |
|---|---|---|---|
| protocol-engine | protocol worker | external/engine/task/util/dlx、core Cargo、lockfile | FTP/Torrent/magnet/Metalink/RTSP/MMS/影音的可用執行路徑、取消與輸出驗證、測試 |
| management-services | management worker | settings/manager/site/grabber/remote/automation/dlxctl 與專屬測試 | 流量模式、站點與資料夾規則、遞迴擷取、遠端控制、單次計劃任務有可操作 API |
| desktop-integration | desktop worker | gui、extension | 批次與拖放、模式/站點/排程/擷取/遠端工具、預覽/分享/記錄，保留既有流程 |
| integration | configured Sol/high integrator | 合併後差異；根代理保留文件/交付 | 一次獨立檢查、修復具體失敗、獨立 verifier、GUI 實際啟動 |

## 驗收

1. 每一個維基功能有對照列與實作入口，區分內建與外部工具。
2. HTTP/Drive/IPC/擴充功能既有回歸不可退步。外部工具使用 argv，不經 shell；失敗不得報完成，取消必須停下子程序。
3. 設定可讀舊版資料；密碼、Cookie 不寫入 history/settings/log。遠端 API 預設關閉，啟用後每次請求驗證 token。
4. 計劃任務的外部程式/關機僅在使用者明確建立的動作執行；驗收不真的關機。
5. 本機 fmt、strict Clippy、release build/test、Node 回歸、CLI smoke、GUI 啟動與實際協定 fixture 分層記錄。
6. 一次獨立 review，必要檢查通過後 commit/push；核對遠端 SHA，CI 若尚未完成明列狀態。

不把歷史網站（Google Video/Flash）已停用的服務當成可恢復的下載來源；以當代影音下載入口對齊用途。非 Windows 實機與公開 Internet 跨網路驗證若未執行標 NOT_RUN。
