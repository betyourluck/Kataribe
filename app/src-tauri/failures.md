
### 46. 再起動するたび LLM 設定が別プロバイダに戻る — dev の repo .env が GUI 保存値を隠す
【実測発見 (2026-07-11, ユーザー報告「再起動すると毎回 gemini に戻っている気がする」)】
GUI で LLM 設定を保存しても、dev 起動 (`npm run tauri dev`) のたびに repo .env の
プロバイダへ巻き戻る。ユーザー仮説は「ブラウザ LocalStorage が効いている?」だったが
**LocalStorage は無実** (フロントが持つのは fontScale/lang 等の UI 設定のみ。LLM 設定は
get_llm_config/set_llm_config = プロセス env + app_data/.env 経由)。
【真因】.env の読み込み優先順位。①main.rs の `dotenvy::dotenv()` が cwd から親へ遡って
**repo 直下の .env を先に**読む (dev のみ) ②setup の `dotenvy::from_path` は**非 override**
なので、GUI 保存先 (app_data/.env) は既設定キーを上書きできない → repo .env が毎回勝つ。
GUI で保存し直した瞬間は set_var で効くため「使っている間は正しく、再起動で戻る」という
気づきにくい症状になる (発見が体感報告経由になったのも必然)。配布版は repo .env が無いので
無事 = **dev でしか再現しない系**。
【解】setup を `dotenvy::from_path_override` に変更 — GUI の保存値 = ユーザーの最後の
明示意思を唯一の真実にする。app_data/.env に無いキーは従来どおり repo .env が生きる
(CLI play は repo .env のままで不変)。
【一般化】**設定ソースが複数あるなら優先順位は「ユーザーの明示意思の新しさ」で決める** —
dotenvy の「先に読んだ者勝ち (非 override)」既定は、開発者向け .env とユーザー向け保存値が
同じキー空間を共有した瞬間に意思の逆転を起こす。体感症状「保存したのに戻る」は
永続化の失敗ではなく**読み込み順の敗北**を疑う。
