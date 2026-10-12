<p align="center">
  <a href="README.md">English</a> | <a href="README.zh.md">中文</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.hi.md">हिन्दी</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (BR)</a>
</p>

<p align="center">
  <img src="https://raw.githubusercontent.com/mcp-tool-shop-org/brand/main/logos/offrig/readme.png" alt="offrig" width="400">
</p>

<p align="center">
  <a href="https://github.com/mcp-tool-shop-org/offrig/actions/workflows/ci.yml"><img src="https://github.com/mcp-tool-shop-org/offrig/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://codecov.io/gh/mcp-tool-shop-org/offrig"><img src="https://codecov.io/gh/mcp-tool-shop-org/offrig/graph/badge.svg" alt="Coverage"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
  <a href="https://mcp-tool-shop-org.github.io/offrig/"><img src="https://img.shields.io/badge/Landing_Page-live-blue" alt="Landing Page"></a>
</p>

[![M8venスコア](https://m8ven.ai/badge/mcp/mcp-tool-shop-org-offrig-790exm?v=3bd60e98b65d7124b74c4126c5ce5ce7)](https://m8ven.ai/mcp/mcp-tool-shop-org-offrig-790exm?s=readme)

レンタルされたRunPod GPUで大規模なモデルを実行します。その際、お客様のGPU上で実行されることは決してありません。デスクトップアプリ、CLI、およびエージェント用のMCPサイドカーを、1つのRustライブラリで提供します。

サイドカー機能により、エージェントは、人が設定した予算内で有料セッションを計画し、GPUをレンタルし、役割ごとに分類されたタスクのキューを独立したランナーに渡すことができます。ランナーは、すべてのモデルスロットを常に稼働させ、失敗したチェックに対してのみ修正を行い、キューが空になるとポッドを停止します。監視プログラムは、他のすべての処理が完了した場合でも、計画の期限が切れるとポッドを強制終了します。

## 状況

2026年10月2日と2026年10月3日に実際に動作確認済み。合計で約5ドル。

- **Frontier:** SGLang上でQwen3-Coder-480B（4ビットAWQ）を4つのRTX PRO 6000（合計384GB）で処理。22分で準備完了後、31回の処理を20秒で完了。費用は3.59ドル。
- **Swarmデータ:** 1つのモデルが同時に数百のエージェントにサービスを提供。480Bモデルは512エージェントで1秒あたり4,059トークンを処理。30Bモデルは1枚のカードで256エージェントで1秒あたり10,147トークンを処理。
- **Runner:** 依存関係とレビューのやり取りを含むキュー。誰かが操作しなくても動作し、処理が完了するとポッドがシャットダウン。
- **保証:** ローカルGPUは、すべての処理中にアイドル状態を維持。

2026年10月7日以降、2つのプロジェクトで同時に日常的に使用されており、それぞれが異なる用途に割り当てられています。具体的には、aspire-siのトレーニング実行は`job`個のポッドで、ai-jam-sessionsの音声レンダリングは`jam`個のポッドで行われます。

2026年10月11日、SGLangポッドで`Application startup complete`が記録され、その後、`/health`が7分間503のステータスを維持しました。GPUの使用率は約100%、消費電力は約100Wで、`/v1/models`は200を返しました。その週に稼働していたポッドはCUDA 13.0を使用しており、問題が発生したポッドにはCUDA 13.2が割り当てられました。13.2が原因であるとは示されていません。ピン機能を使用することで、正常に動作したバージョンを維持できます。このリリースでは、その問題が報告されており、いずれかの処理が完了し、結果が返されるまで、レシピは「準備完了」とは見なされません。プロファイルでは、ホストのCUDAバージョンを正確に指定することもできます。

今回のリリースでは、レンタル環境のポッドで以下の点が検証されていません：1つのトークンで実行可能な状態であるかのチェック、`engine_unresponsive`および`engine_hung`のレポート、および`gpu.allowedCudaVersions`を送信する処理。これらの処理は、偽のSSHセッションと偽のRunPodに対するテストでカバーされています。再検証のためにポッドは起動されませんでした。RunPodのドキュメントに記載されているv1`POST /pods`の列挙型（2026年10月11日に確認）は、依然として13.0で停止しています。13.1と13.2は、そのライターポッドが13.2に更新されたため、offrigのリストに含まれています。依然として、最低限の処理では`gpu.minCudaVersion`のみが送信されます。

構築とテストは完了しており、あとは決定を待つだけです。ネットワークボリュームにフロンティアモデルの重みを配置する作業について、月額約21ドルです（詳細については「フロンティアモデルの重みをネットワークボリュームに配置する」を参照）。

次：本格的なフロンティア・キューの最初の実装。これは、リリース前に完全に計画され、コードはコンパイルされ、ポッド上でテストされる。

## その機能は何か

1つのウィンドウ（または1つのコマンド）から、オフリッグ：

1. 現在のRunPod残高、GPUのリアルタイム価格、および残高がどのくらい持つかを表示します。
2. Ollamaでは1つの小規模GPUから、SGLangでは最大4つのRTX PRO 6000まで、指定されたスペックのポッドを起動し、そのポッドにモデルを読み込みます。
3. ポッドへのSSHトンネルを開きます。
4. ポッドのモデルをZedに独自のプロバイダーとして追加します。
5. モデルがこのマシンで正常に動作するかどうかを7つのチェックで確認します。
6. ポッドを停止するか、すべてのGPUがアイドル状態になった後、一定期間経過したらポッドを終了します。

サイドカーを通じて、エージェントは予算内でセッションを計画し、圧縮や再起動を挟んでもプロジェクトの情報を保持し、さらに、手動操作なしでハンドオフキューを実行します（詳細は「エージェント用のサイドカー」を参照）。

## 保証の内容と、その保証がどのように機能するか

- **モデルサーバーは、トンネル経由でのみアクセス可能です。** ポッドは、ポッド自身のループバックにバインドされた、Ollama（`ollama/ollama:0.35.0`）またはSGLang（`lmsysorg/sglang:v0.5.20-cu130`）という特定のエンジンを実行し、ポッドは`22/tcp`のみを公開します。公開されているHTTPエンドポイントはなく、そこからアクセスしたり悪用したりすることはできません。レシピを使って、エンジンをループバックから移動させることはできません。
- **Zedは、独自のポートでトンネルと通信します。** トンネルは`127.0.0.1:11435`でリッスンします。ローカルのOllamaは`11434`で実行されています。offrigは、トンネルを`11434`に配置することを拒否するため、接続が切断されたトンネルはローカルサーバーに到達できず、代わりにリクエストが失敗します。
- **Zedは、プロバイダーを切り替えることはありません。** ポッドのモデルは、Zed内の別の`offrig`プロバイダーとして扱われます。ポッドがダウンしている場合、それらのいずれかを選択するとエラーが発生し、Zedは別のプロバイダーを試しません。
- **モデルの重みはローカルに保存されません。** モデルはポッドにプッシュされ、ポッドによって（またはHugging Faceからダウンロードされるか、ステージングされたネットワークボリュームから読み込まれます）。

警備員は、その都度、実際に確認を行い、現場で観察できる事実に基づいて検証する。

| 確認する | 失敗する場合： |
|---|---|
| トンネル接続を使用することで、ローカルのOllamaポートを回避できます。 | トンネルポートは11434です。 |
| ゼッドは、トンネルを通してポッドの模型を送る。 | ZedのプロバイダーURLは、トンネルとは全く異なるものです。 |
| PodのOllamaはインターネットに接続されていません。 | ポッドはポート11434を外部に公開する。 |
| トンネルの先には、ポッドがある。 | トンネル経由で表示されるモデルのリストは、SSH経由でポッドから読み出されるリストとは異なる。 |
| この機械にはポッドモデルは搭載されていません。 | ローカルのOllamaにも、ポッドモデルが存在します。 |
| どのポッドモデルも、ローカルのゼッドモデルと共通の名前は持たない。 | オフリグプロバイダーに登録されている名前が、ゼッドのローカルのOllamaリストにも含まれている。 |
| ゼッドが提供するすべてのモデルは、ポッドに搭載されています。 | ゼッドは、このポッドにはないモデルを提供している。 |

## インストールする

OpenSSH（標準搭載）が利用できるWindows環境、エディターでモデルを扱いたい場合はZed、そしてRunPodのアカウントが必要です。

1. `offrig-<version>-windows-x64.zip` を [リリース](https://github.com/mcp-tool-shop-org/offrig/releases) からダウンロードし、リリースの `SHA256SUMS` と照合して、`PATH` に解凍します。これには、`offrig.exe`（CLI）、`offrig-app.exe`（アプリ）、および `offrig-mcp.exe`（サイドカー）が含まれます。
ソースコードからビルドする場合は、次のコマンドを実行します。`cargo build --release`。Rust 1.98.1 を使用します（`rust-toolchain.toml` で固定）。
2. RunPod API キーをユーザー環境変数 `RUNPOD_API_KEY` に設定します。
3. RunPod のアカウント設定に SSH 公開キーを追加します。offrig は、存在する場合は `~/.ssh/runpod_rustline`、次に `~/.ssh/id_ed25519` を使用します。
4. エージェントの場合、サイドカーをユーザーのスコープで Claude Code に登録します。`claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`。これにより、プロジェクトのストアが初回使用時にのみ開かれるため、使用しないプロジェクトでは問題ありません。

[ハンドブック](https://mcp-tool-shop-org.github.io/offrig/handbook/) には、最初のポッド、サイドカー、構成、レーン、およびジョブポッドに関する手順が記載されています。

## 使用方法

**アプリ:** `offrig-app` を起動し、プロファイルを選択して、[ポッドの起動] をクリックします。準備が整うと、モデルが Zed のエージェントパネルに「RunPod · …」として表示されます。初回起動後、Zed を 1 回再起動すると、`OFFRIG_API_KEY` が認識されます。

**CLI:**

```text
offrig status                 balance, runway, pods
offrig gpus --count 2         live offers for a GPU count
offrig profiles               tiers and their models
offrig up medium              launch, pull, wire Zed, run the checks, hold the tunnel
offrig up frontier --wait 180  wait up to 3 hours for the GPUs, renting nothing meanwhile
offrig tunnel medium          hold the tunnel to a running pod
offrig check gpt-oss:120b     streamed chat with a tool call, the way Zed sends it
offrig guard                  run the seven checks
offrig pull <model>           pull another model onto the pod
offrig connect                open the pod's /workspace in Zed for remote editing
offrig down medium --yes      terminate the pod
offrig zed-remove             take the provider out of Zed
offrig budget 15              set this project's spending cap for agent sessions (human only)
offrig index src docs           index this project for hybrid search (CPU embeddings)
offrig verify calibrate gold.jsonl --model gemma4:31b   score a local verifier model
offrig stage frontier --dc EUR-IS-1 --yes   stage weights on a network volume (bills monthly)
```

`offrig --help` と `offrig <command> --help` には、すべてのコマンドとフラグがリストされています。上記のブロックは、毎日の使用に必要なものです。`offrig init`、`offrig models`、`offrig zed`、および `offrig up --no-zed` は、[ハンドブック](https://mcp-tool-shop-org.github.io/offrig/handbook/usage/) に記載されています。

### 出力、終了コード、およびエラー

- **ログレベル:** `-q` は、エラーとコマンド自体の結果のみを出力します。`-v` には、各 RunPod 呼び出しとそのタイミングが追加されます。`--debug` には、失敗したレスポンスの本文と完全なエラーチェーンが追加されます。
API キーは、すべてのレベルでマスクされます。
- **終了コード:** `0` は成功、`1` は修正が必要な問題（引数、構成、ガードまたは予算の拒否、キーの欠落）、`2` は実行時のエラー（RunPod、ネットワーク、ssh、タイムアウト、容量不足）を示します。
- **サイドカーエラー** は結果であり、プロトコルエラーではありません。`ok:false` には、安定した `code`、`error` テキスト、`next_action`、および `retryable` が含まれます。コードは、[ハンドブックのリファレンス](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/) にリストされています。

## サイドカー（エージェント用）

`offrig-mcp` は、Claude Code などのエージェントが使用する MCP サーバーです。各プロジェクトに `<project>/.offrig/offrig.db` にデータベースを保持し、すべてのポッドよりも長く存続するため、セッションは圧縮または再起動後も、再説明なしに維持されます。

| ツール | その機能は何か。 |
|---|---|
| `offrig_status` | プロジェクト、予算、RunPod の残高と残りの実行時間、offrig のポッド、各オープンプランとその `plan_id`、レーン名、およびポッド名、保留中の作業がフラグ付けされたハンドオフキュー。各ポッドには、RunPod レコードから取得した `gpu_count` と `gpus`（`2x <type>`）に加えて、プランの `planned_gpu_count` が含まれます。 |
| `offrig_offers` | GPU 数に対するライブ GPU オファー |
| `offrig_plan` | セッションの最悪の場合（ライブ価格 x 最大時間）で価格を設定し、残りの予算を超えて拒否します。応答には、レーンの `ssh_alias` と起動時に作成される `pod_name`、および要求される `container_disk_gb`（オプションの `container_disk_gb` はプロファイルのものをオーバーライドします。「コンテナディスク」を参照）が含まれます。オプションの `max_price_hr` と `no_fallback` は、レンタルできる GPU を絞り込みます（「プランのハードウェアを固定する」を参照）。オプションの `wait_minutes` は、容量がない場合に起動が再試行する回数を設定します（「容量を待つ」を参照）。 |
| `offrig_memory_search` | アクティブなプロジェクトメモリを検索し、各結果にはソースと日付が含まれます |
| `offrig_memory_record` | 簡潔な説明、制約、決定、事実、またはチェックポイントを追加します。変更は、理由とともに以前のものを置き換えます。 |
| `offrig_handoffs` | 役割に基づいてハンドオフをキューに入れ（それぞれに受け入れチェックが必要です。オプションの決定的なチェック）、リストし、役割ブロックをプレビューし、ハンドオフの最良の出力を表示します（また、`.offrig/out/` にも書き込まれます）。結果（完了、無効、違反、失敗、フィードバックによる再試行）を記録します。 |
| `offrig_launch` | **Spends.** Takes only a `plan_id`: commits the worst case, waits for GPUs renting nothing, boots the pod, opens the tunnel, pulls the models, starts the watchdog. Idempotent per plan. Refused while the lane already has a live plan or pod: `lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | 起動の進行状況（ポッドが起動している間、ステップは呼び出し時のポッドの状態から派生し、`progress.pod_status` は生の v2 ステータスです。各容量の再試行は `progress.capacity_wait` にカウントされます）、GPU の種類、`gpu_count`、`gpus`、`planned_gpu_count`、および ssh が確立された後の `measured_gpu_count`（カードがない場合の読み取りは `0` です）。`gpu_mismatch` は、レコードまたは測定値がプランと異なる場合に true になります。ホストの CUDA バージョンは `nvidia-smi` で測定され、ホストがプランの CUDA 最小値よりも古い場合は、大きな `warnings` エントリが出力されます。レシピの場合、`ready` は 1 つの完了で選択肢が返されたことを意味します。`engine_hung` と `engine_unresponsive` は、ポッドを起動したままにします。監視プログラムの生存時間、残りの時間、これまでの消費量 |
| `offrig_ask` | ポッドモデルでのハンドオフの 1 ターン。コンテキストは、プロジェクトストアから構築されます。応答は、信頼できない出力として返されます。 |
| `offrig_run` | すべてのモデルスロットをビジー状態に保つ、分離されたランナーを開始します。準備ができた各ハンドオフをドラフトし、失敗したチェックに対して最大 2 回再検討し、結果を依存するハンドオフにフィードし、キューが空になるとポッドをシャットダウンします（ただし、`keep_pod` の場合を除く）。コードでチェックできない作業は、レビュー待ちとして残ります。 |
| `offrig_put` | ローカルファイルまたはディレクトリをジョブポッドにコピーします（scp）。相対ポッドパスは `/workspace/job` の下にあります。オプションの `plan_id`（以下を参照）。 |
| `offrig_exec` | bash コマンドをジョブポッドで実行し、分離してサイドカーよりも長く存続させます（`start`）。実行中または終了したコマンドを、終了コードとログの末尾（`status`。`save_log` は、ログ全体をローカルファイルにコピーします）とともに報告します。コマンドを終了します（`stop`）、または短いコマンドを今すぐ実行し、stdout、stderr、および終了コードを返します（`run`、`timeout_secs` のデフォルトは 30、最大 120）。オプションの `plan_id`（以下を参照）。 |
| `offrig_get` | ファイルまたはディレクトリをジョブポッドからローカルにコピーし、不足しているローカルの親フォルダーを作成します。シャットダウン前に実行します。シャットダウンでは、ポッドのディスクが削除されます。オプションの `plan_id`（以下を参照）。 |
| `offrig_shutdown` | **ポッドを破棄します。** ポッドを終了し、プランの会計を測定された消費量で閉じます。ハンドオフが進行中の場合は、理由が与えられない限り拒否されます。レーンがそのポッドに名前を付けた場合（`ssh_block_removed`）、レーンの独自の `~/.ssh/config` ブロックを削除します。 |
| `offrig_complete` | **消費します。** 許可されたプロジェクトとモデルでのみ、OpenRouter の 1 回のチャット完了。最悪の場合を予算に対してコミットし、回答をプロジェクトファイルに書き込み（すでにそのパスが存在する場合は置き換えます）、実際の料金を記録します。 |

**ジョブツールが適用されるジョブプランはどれですか。** `offrig_put`、`offrig_exec`、および`offrig_get`はオプションの`plan_id`を受け取ります。正確に1つのジョブプランが実行中で、`plan_id`がない場合、以前と同様にそれを使用します。1つ以上のジョブプランが実行中で、`plan_id`がない場合、それらを拒否し、実行中のプラン（ID、プロファイル、ポッド名）をリストします。それらは決して推測しません。`plan_id`がある場合、そのプランのポッドに対してのみ動作し、ポッド名がそのプランが所有するものであることを確認した後にのみ動作します（別のレーンのポッドは拒否され、単に別のレーンのポッドではありません）。すべてのジョブツールの応答と`offrig_job`は、動作した`project`と`plan_id`を示します（ジョブツールの応答には`lane`も含まれます）。`offrig_status`は、`project`を示し、実行中のすべてのプランとその`plan_id`、レーン、およびポッド名をリストします。

ロールは、Role OS（ドキュメントとスターターパックカード）と、Role OSの形式で提供される4つのゲームロール（ゲームデザイナー、システムデザイナー、ナラティブデザイナー、ローアキーパー）から構成されます。予算の上限は、人間によってのみ設定されます。

```text
offrig budget --provider runpod 30       RunPod's cap (run in the project directory)
offrig budget --provider openrouter 8    OpenRouter's cap
offrig budget --show                     every cap, beside each provider's own balance
```

すべてのサイドカーの起動は、**ウォッチドッグ**を開始します。これは、エージェント、セッション、またはサイドカーが存在しなくても、プランの期限（コミットされた時間+最大時間）でポッドを終了する別のプロセスです。失敗した検索に対しては動作せず、正確に1回終了し、会計処理を閉じ、`.offrig/watchdog-<plan>.log`にログを記録します。エンジンがサービスを開始する前に、レンタルされたポッドの準備が失敗した場合、サイドカーはポッドを課金対象から削除します。
`engine_hung`と`engine_unresponsive`は、すべてのインターフェースでレポート専用です。サイドカー、CLI、およびアプリはポッドを起動したままにし、準備完了のメッセージは表示しません。CLIとアプリでは、コマンドはエラーで停止します。サイドカーでは、起動結果は成功した状態のままなので、ポッドは削除されず、プランはコミットされた状態のままになり、ジョブは`running`のままです。すでに終了したエンジンプロセスは、そのレポートではありません。サイドカーでは、これは失敗した起動であり、補償メカニズムがポッドを削除します。`offrig up`またはアプリで開始されたポッドには、ウォッチドッグはありません。コマンドがエラーで停止した場合（ハング、応答しないエンジン、終了したエンジン、またはタイムアウト）、ポッドは`offrig down <profile> --yes` or the app's Shut down. A pod RunPod reports as `ERROR` or `EXITED`が例外となるまで、課金対象のままになります。この場合のみ、すべてのインターフェースで終了します。

設計とその証拠は、[docs/sidecar-design.md](docs/sidecar-design.md)にあります。

### プロジェクトインデックスと検証者

```text
offrig index src docs                 chunk this project; only changed files are re-embedded
offrig index --status                 counts, model, dimension; changes nothing
offrig index --rebuild                drop every vector and embed again
offrig verify calibrate gold/grounded.jsonl gold/reasoning.jsonl --model gemma4:31b --think on
offrig verify calibrate --think-policy grounded=off,reasoning=escalate:off>on --model gemma4:31b gold/grounded.jsonl
offrig verify calibrate --report-only .offrig/out/calibrate-<model>-<time>
```

`offrig index`は、CPUのみのOllama（`embed_url`、デフォルトは`http://127.0.0.1:11490`）とともに組み込まれ、共有サーバー（11434）やGPUは使用しません。プロジェクトにインデックスが設定されると、`offrig_memory_search`はデフォルトでハイブリッド（キーワードと意味）になります。
`offrig verify calibrate`は、ローカルループバックモデルを検証者としてスコアリングします。1回の呼び出しにつき1つのゴールドクレームを使用し、そのクレーム自身の証拠を使用します。"cloud"という名前を含むモデルは拒否されます。
`--num-ctx`はデフォルトで`auto`になり、1つのウィンドウのサイズを設定します。`--keep-thinking`は、実行ディレクトリ内の`thinking.jsonl`に思考テキストを書き込みます。
[ハンドブックリファレンス](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/)には、フラグがリストされています。

### レーン：プロジェクトごとに1つのサイドカー、競合なし

1つのRunPodアカウントで、2つのプロジェクトが同時にサイドカーを実行できます。各プロジェクトは、他のプロジェクトと共有しない独自の**レーン**（SSHエイリアス、トンネルポート、ポッド名タグ）を取得します。

| | プレーンレーン（CLI、アプリ、Zed） | プロジェクトのレーン |
|---|---|---|
| SSHエイリアス | `offrig` | `offrig-<tag>` |
| トンネルポート | `11435`（ランナー`11436`） | `11500`、`11502`、...のうち最初に使用可能なもの（ランナー：上記のポート） |
| サイドカーポート（シェルドライバー） | なし | `11700` + レーンのスロット：`11700`、`11701`、... |
| ポッド名 | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| SSHブロック | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>`は、プロジェクトフォルダーの名前（`aspire-si`、`ai-jam-sessions`）から取得され、2つのプロジェクトが同じフォルダー名を使用する場合、短いハッシュが追加されます。プロジェクトのレーンは、最初にセッションを計画したときに割り当てられ、offrigの構成ディレクトリ内の`lanes.toml`に書き込まれ、保持されます。同じプロジェクトは、再起動後も同じレーンを取得します。割り当てにはロックファイルが使用され、レジストリはアトミックに書き込まれるため、同時に開始された2つのサイドカーが同じタグ、エイリアス、またはポートを共有することはありません。どのレーンも`11434`（ローカルOllamaのポート）にすることはできません。範囲は`11500`から開始され、それ以外を指定するように編集されたレジストリは拒否されます。プランは、そのレーンを記録し、その起動、ランナー、ウォッチドッグ、およびシャットダウンはすべて、グローバル構成ではなく、そのレーンを使用します。

サイドカーは、常に自身のレーンに名前が付けられたポッドとのみ一致、リスト、または停止します。別のプロジェクトのレーン、プレーンレーンの`offrig-<profile>`ポッド、およびアカウント上のその他のポッドはそのままにします。ライブポッドの起動チェックは、自身のレーンのみを要求し、シャットダウンは、プランのレーンではないポッド名を拒否し、トンネルの孤立チェックは、そのフォワードとエイリアスが自身のレーンである場合にのみ、古い`ssh`を終了します。レーンが存在する前に作成されたプランは、レーンが記録されておらず、プレーンレーンで実行され続けるため、古いスキームで起動されたポッドは、それを開始したのと同じプランによってシャットダウンされます。

**サイドカー自身のポート。** `offrig-mcp`は、stdio経由でMCPと通信します。セッション全体で1つの接続を開いたままにする（セッション自身のMCP接続が不安定な場合）シェルドライバーは、それをループバックHTTPポートの背後に配置し、そのポートは以前はマシン全体で1つの番号でした（`11439`）。2番目のプロジェクトのドライバー、またはその他のプログラムがそれを取得すると、最初のサイドカーは何も言わずに停止しました。現在は、プロジェクトごとにデフォルト設定されており、トンネルポートと同様に、プロジェクトのレーンから取得されます。レーンスロット`i`（トンネルポート`11500 + 2i`）には、サイドカーポート`11700 + i`が割り当てられます。
範囲`11700`から`11763`は、レーンが持つことができるすべてのトンネルポートとランナーポート（`11500`から`11627`）、プレーンレーンの`11435`と`11436`、およびローカルOllamaの`11434`よりも上にあります。したがって、サイドカーポートはトンネルポートになることはありません。新しいものは保存されません。`lanes.toml`は変更されず、ポートはレーンから派生します。`OFFRIG_SIDECAR_PORT`は引き続きそれをオーバーライドします。ポートではない値、1024未満の値、または`11434`、`11435`、`11436`、またはレーントンネル範囲内の値は拒否されます。

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

`--check`を使用すると、使用中のポートはエラーとなり、ポート名が表示されます。また、オフリグのサイドカーが応答した場合、そのサイドカーがサービスを提供するプロジェクトも表示されます。「サイドカーポート11700は使用中です。オフリグのサイドカーがすでに<path>のプロジェクトをサービスしています。最初にそれを停止するか、OFFRIG_SIDECAR_PORTを空いているポートに設定してください」。このチェックは、ドライバーがすでにどのように応答するか（誰もサービスを提供していないプロジェクトのリクエストであり、ドライバーはツールに触れる前にそれを拒否します）を確認するため、実行中のサイドカーには何も変更がありません。`offrig_status`は、レーンの`sidecar_port`を報告します。

**1つのレーン、1つのアクティブなポッド。** レーンには1つのSSHエイリアスと1つのトンネルポートがあるため、一度に1つのポッドをサービスします。レーン内の2番目のポッド（`offrig-<tag>-job`は`offrig-<tag>-jam`の隣）は、エイリアスを自身に再設定し、最初のプランの`offrig_put`、`offrig_exec`、および`offrig_get`を間違ったマシンに送信します。したがって、`offrig_launch`は、レーンに開いているプランまたは所有するアクティブなポッドがある場合に拒否します。「レーン<tag>にはアクティブなポッド<name>（プラン<id>）があります。最初にそれを停止してください`, before anything is committed or rented. The plain lane's `オフリグを起動します」。アプリも、別のプロファイルのポッドに対して同じように拒否します（同じプロファイルのポッドは引き続き再利用されます）。最初に最初のプランを停止してから、次のプランを起動します。

## ティア

プロファイルは`%APPDATA%\offrig\config.toml`に保存されます（最初に変更されたときに書き込まれます）。デフォルト：

| プロファイル | GPU | モデル | 典型的なコスト |
|---|---|---|---|
| 小 | 1 × RTX 2000 Ada / A4000クラス | `qwen3:4b` | 約$0.25/時間 |
| 中 | 1 × RTX PRO 6000（96 GB）。空きがない場合は、A100またはH100 80 GB | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | $2.09/時間（A100フォールバック$1.59） |
| 最先端 | 4 × RTX PRO 6000（384 GB）、**SGLang** | Qwen3-Coder-480B AWQ 4ビット（252 GB）、コンテキスト用に約130 GB残っています | $8.36/時間 |
| 最先端-ミニ | 1 × RTX PRO 6000、**SGLang** | Qwen3-Coder-30B FP8（31 GB）：最先端エンジンパス、低コストでリハーサル | 約$1.7/時間 |
| 最先端-ミニ-awq | 1 × RTX PRO 6000、**SGLang** | Qwen3-Coder-30B AWQ（17 GB）：最先端の4ビットMoEカーネル、低コストでリハーサル | 約$1.7/時間 |
| ジョブ | 1 × RTX PRO 6000（96 GB）。空きがない場合は、A100またはH100 80 GB | なし：**ジョブポッド**は、モデルサーバーではなく、実際の作業を実行します | $2.09/時間（A100フォールバック$1.59） |
| ジャム | 最初に1 × A40（48 GB）。空きがない場合は、A6000、A5000、3090、L4、または4090 | なし：ai-jam-sessionsの歌のレンダリング（SoulX-Singer）用の**ジョブポッド** | $0.49/時間（A40） |

`recipe`を持つプロファイルは、Ollamaとは異なるエンジンを実行します：ピン留めされたイメージ（`lmsysorg/sglang:v0.5.20-cu130`）、起動時にダウンロードするHugging Faceモデル、および追加のサーバー引数。オフリグは、GPU数からテンソル並列処理、プロファイルからコンテキスト長を設定し、エンジンをポッドのループバックに保持します。レシピはそれらをオーバーライドできません。ゲートされたリポジトリの場合、`hf_token_secret`はRunPodシークレットの名前を示し、`{{ RUNPOD_SECRET_<name> }}`として参照されるため、トークンはポッド仕様に入ることがありません。起動時に、エンジンがそれらをダウンロードしている間に、ディスク上の重みを報告し、エンジンが終了するとすぐに停止します（エンジンのログ付き）。`/health`が応答したときにエンジンが準備完了と見なされず、`/v1/models`が応答することも準備完了ではありません。`/health`が200を返した後、オフリグは1つのチャット補完（`max_tokens` 1、`temperature` 0）を送信し、最大30秒間待ちます。その応答に選択肢がある場合にのみ、そのステップが`ready`になります。補完が失敗するかタイムアウトした場合、オフリグは15秒後に再度試みます。2回目の失敗は、ステップ`engine_unresponsive`です。ジョブは実行を続け、エラーとエンジンのログはジョブに記録されます。次のアクションは、「エンジンが補完に応答していません。ポッドを停止するか、まだウォームアップしている場合は待ちます」です。オフリグは、サイドカー、CLI、またはアプリで、このためにポッドを停止しません。最初のリクエストが遅い（グラフキャプチャ、JIT）ことは、ダウンロードとロードを破棄する理由ではありません。プランの期限はコストを制限します。ハングクロックは、ログテールと同じSSH読み取りの`grep -c`の`Application startup complete`から開始され、エンジンのログ全体にわたって、最後の40行のみではありません。会話の多いエンジンは、起動行をそのテールから押し出す可能性があります。カウントは引き続きクロックを開始します。そのカウントが少なくとも1であり、`/health`が3分間ダウンしている場合、ステップは`engine_hung`になります。最後の40行、カウントが最初に確認されてからの秒数、および次のアクション`shut the pod down: the engine finished startup but never became healthy`です。
オフリグは、そのハングを報告し、ポッドを起動したままにします。ポッドを停止することはあなた次第であり、GPU数の不一致と同じです。GPU数の警告は、次のアクションまたは応答しないアクションの先頭に付加されないため、文は正確なままです。すでに終了したエンジンプロセスも報告されません。サイドカーでは、補正器がそのポッドを削除します。CLIとアプリでは、コマンドはエラーで停止し、それを削除しません。

各プロファイルは、優先順位の順にGPUタイプをリストします。v2は1つのタイプを配置し、フォールバックしません。したがって、オフリグは、プランの価格を超えないように、そのリストを順に試します。空きがない場合、プロファイルは待機できます（`wait_for_gpu_minutes`。最先端は最大120分まで待ちます）。オフリグは1分ごとにチェックし、GPUが空きになった瞬間にポッドを作成します。待機中は何もレンタルされません。Ctrl+Cまたはアプリのキャンセル起動で停止し、ライブカタログがダウンしている場合は、各分ごとに作成を再試行します。大規模なマルチGPUセットアップは、数分以内に開始および終了します。価格は、v2カタログのセキュアクラウドリスト価格（`GET /catalog/gpus`）、GPUあたり、GPU数に掛けたものです。NONEのリストは無料ではありません。公開価格ページは、利用可能な価格ではありません。`CUDA_VERSIONS`は、GPUの`cudaVersions`リストが省略された場合のフォールバックにすぎません。

### 容量を待機中

プランが`no_fallback`または`max_price_hr`に絞られると、必要なリソースが不足することが多いため、ジョブは失敗する代わりに静かに再試行し、実行中に何もレンタルしません。再試行までの待機時間は、次の順序で決定されます。まず、プランの`wait_minutes`（プランに保存されている`offrig_plan`引数。`0`は即座に失敗します）。次に、プロファイルの`wait_for_gpu_minutes`です。`job`プロファイルでは、デフォルトで20分に設定されます。待機時間は、プランに残された時間から5分の猶予時間を差し引いた時間に短縮されるため、プランの期限を過ぎることはありません。また、待機中に何もレンタルされないため、最悪の場合のコストに影響はありません。`offrig_launch`は`capacity_wait_minutes`を報告します。待機中は、`offrig_job`に`progress.capacity_wait`（`checks`、`waited_secs`、`limit_secs`）と、どのチェックが行われているかを示すステップが表示されます。待機時間が終了すると、ジョブは`no capacity`で失敗し、何もレンタルされません。

### プランのハードウェアを固定する

プロファイルは、GPUの種類を優先順位順にリストし、RunPodは利用可能な最初のGPUを選択します。制限がない場合、プランはメモリが少なく、古いドライバーが搭載され、価格が異なるGPUに割り当てられる可能性があります。3つの制限により、プランは実際に使用できるハードウェアに制限されます。プランニング自体は無料ですが、制限はプランがレンタルできるものを絞り込むだけです。

| 制限 | 場所 | 効果 |
|---|---|---|
| `min_cuda` | プロファイル（`config.toml`） | RunPodのリストにある最も古いホストのCUDA（ドライバー）バージョン（`13.2`、`13.1`、`13.0`、`12.9`、... `11.8`）。ポッドの作成時に、この最小値が`gpu.minCudaVersion`として送信されるため、最も新しいコピーされたバージョンよりも新しいホストでも条件を満たします。カタログがGPUに対して`cudaVersions`を返した場合、プランはそのタイプを、この最小値以上のバージョンが利用可能な場合にのみ保持します。カタログにリストが含まれていない場合、コピーされたリストが代替として使用されます。ジョブプロファイルの場合、この値とイメージ自体の`[profiles.job] min_cuda`のうち、より新しい方が適用されます。 |
| `min_vram_gb` | プロファイル | プランが受け入れる最小の合計VRAM（プロファイルのすべてのGPUの合計）。それ以下のオファーは拒否され、RunPodがメモリ情報をリストに含めていないタイプも拒否されます。 |
| `max_price_hr` | `offrig_plan`引数 | ポッドの総コストの最大値（すべてのGPUの合計$/時間。この値は`offrig_offers`に表示されます）。それ以上のオファーは拒否され、現在価格がリストにないタイプも拒否されます（上限に準拠することはできません）。 |
| `no_fallback` | `offrig_plan`引数 | プロファイルの最初のGPUファミリーのみが許可されます。2つのRTX PRO 6000 Blackwellエディション（ServerとWorkstation）は1つのファミリーであり、A100 SXMやPCIeなど、他のすべてのカードはそれぞれ独自のファミリーです。 |

プロファイルフィールドはオプションであり、デフォルトでは設定されていないため、以前のoffrigによって記述された`config.toml`は変更されずにロードされます。`job`プロファイルは`min_cuda = "13.0"`を設定します。

`cuda_versions`は、`min_cuda`に加えて、オプションの正確なリストです。たとえば、`cuda_versions = ["13.0"]`です。設定されている場合、ポッドの作成時に、リストが記述された順序で`gpu.allowedCudaVersions`として送信され、最小値も送信されません。`offrig_plan`はリストを報告します。これを`min_cuda`（ジョブイメージ自体の最小値を含む）と組み合わせて設定することはできません。2つの値を組み合わせると、曖昧になります。各バージョンは、offrigが受け入れるものでなければなりません。リストは、最新のものから順に、13.2、13.1、13.0、次に12.9から11.8までです。13.2と13.1は、RunPodがドキュメント化しているv1列挙型（2026-10-11に読み取り、13.0で停止）よりも進んでいます。これらは、2026-10-11に13.2ホストに配置されたライターポッドがあるため、リストに表示されます。最小値は、常に`gpu.minCudaVersion`のみを送信します。

`offrig_plan`は、残っているリソースから最悪の場合のコストを計算します。
`max_hours x min(max_price_hr, the dearest listed price among the remaining GPUs)`。
`max_price_hr`がない場合、これは以前と同様に、プロファイルにリストされている最も高価な価格になります。何も残っていないプランは、拒否され、各GPUが拒否された理由が通知され、何も書き込まれません。

The plan stores the GPU list that is left, and either the CUDA floor or the exact CUDA
list, and `offrig_launch` rents only from what the plan stored, never from a later edit
of the profile. `offrig_job` and the side-car's launch result
report the GPU type and the host's CUDA version actually rented, in a `rented` object. The
pod API does not report the host's CUDA version, so once ssh is up the launch runs
`nvidia-smi` once through the lane and reads it from the header (`CUDA Version: 12.8`, or
`CUDA UMD Version: 13.4` on newer drivers); `rented.cuda_source` says `nvidia-smi` or
`pod API`. If the host's CUDA is older than the plan's floor, the GPU is
not one the plan listed, or the price is above the plan's, `offrig_job` returns a `warnings`
entry and starts `next_action` with `WARNING`. Nothing is terminated automatically: stopping
the rent is the caller's call (`offrig_shutdown`). If neither the pod API nor `nvidia-smi`
gives a CUDA version, `rented.notes` says so and the floor is unchecked.

### コンテナディスク

ポッドには、ホストにローカルなコンテナディスクと、`/workspace`にマウントされているボリュームという2つのディスクがあります。一部のホストでは、`/workspace`は速度の遅いネットワークファイルシステムです。あるジョブポッドでは、32MB/sが測定され、コンテナディスクでは354MB/sであり、約130GBのモデルを時間内に取得できませんでしたが、コンテナディスクはわずか60GBでした。コンテナディスクのサイズは、プロファイルの`container_disk_gb`（組み込みプロファイルでは30〜60GB、`job`プロファイルでは60GB）であり、ポッドの作成時に`disk`として送信されます。`offrig_plan`は`container_disk_gb`（1〜2000）を受け取り、1つのプランに対してこれを上書きします。プランはこれを保存し、起動はこれを送信し、プランの応答と`offrig_status`は、実際に使用されているサイズを表示します。

- The container disk is **not priced**: offrig prices GPU time only, so the plan's worst
  case is the same with any size. RunPod charges for disk; whether the rate it reports for
  the pod (`offrig_job` shows it) includes the container disk has not been checked here.
- offrig does not move your downloads for you. Job commands start with `HF_HOME` on the
  `/workspace` volume (`/workspace/hf`); to use the container disk, set your own
  (`HF_HOME=/root/hf python ...`) in the command.
- The container disk is deleted with the pod, like the volume without a network volume:
  copy results back with `offrig_get` before `offrig_shutdown`.
- The 1 to 2000 bound is offrig's own sanity check against a typo; RunPod's actual limit is
  not checked.

### ジョブポッド

`job`を持つプロファイルは、トレーニング実行など、その上で実行される作業のためにGPUをレンタルします。モデルの提供のためではありません。そのポッドは、ピン留めされたPyTorchイメージ（`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`、Blackwell用のCUDA 12.8）を実行し、sshdとそれ以外に何も含まれていません。

- It serves no model, so there is no tunnel and nothing is wired into Zed. sshd allows no
  forwarding at all (`AllowTcpForwarding=no`); the only way in is ssh to the pod.
- A job profile lists no models and cannot also have a recipe; the config check refuses
  either.
- `offrig up` and the app refuse a job profile before renting anything. It runs through
  the side-car: `offrig_plan profile=job`, `offrig_launch`, then `offrig_put`,
  `offrig_exec` and `offrig_get`. The launch is ready when sshd answers.
- A command runs detached on the pod (`setsid nohup`) in `/workspace/job`, so it outlives
  the side-car and the ssh session. It is sent as base64, so nothing in it is read by the
  ssh shell. Its log and exit status stay in `/workspace/offrig/jobs/`. Hugging Face
  downloads go to `/workspace/hf` on the pod volume.
- `offrig_exec action=run` is for quick checks (`ls`, `nvidia-smi`), not work: it runs the
  command to completion under `timeout` (default 30 s, at most 120 s) and returns `stdout`,
  `stderr`, `exit_code` and `timed_out`. Output is cut to the last 64 KB of each stream
  (`truncated`) and is untrusted pod output. A command that needs longer is a `start`.
- A job's log tail has progress bars collapsed: tqdm-style redraws joined by carriage
  returns show only their last frame. `offrig_exec action=status save_log=<local path>`
  also copies the job's whole log, as written, to a local file (parent folders are created),
  so the tail can stay short.
- The image is a CUDA 12.8 build, so a job profile names the oldest host CUDA version it
  runs on (`min_cuda = "12.8"`) and the pod is created with that floor as
  `gpu.minCudaVersion`. Without that, a host with an older driver starts the pod and torch finds no GPU,
  after the rent has begun. The work itself can need more than the image: the `job` profile
  also sets `min_cuda = "13.0"` on the profile (see above), because the jobs it runs install a
  current vLLM, whose PyTorch is a CUDA 13 build.
- Budget, plan, watchdog and shutdown work as for every other profile. Copy results back
  before `offrig_shutdown`: the pod's disk goes with it.
- `jam` is the job profile ai-jam-sessions renders its singing on: SoulX-Singer needs far
  less than a training card, so it rents a cheap 24-48 GB one. The setup and the session
  live in that repository (`docs/vocal-offrig.md`); offrig knows nothing about singing.

### ネットワークボリュームへの重みのステージング

レシピプロファイルは、起動時に毎回重みをダウンロードします。以前のバージョンでは、準備完了までに約20〜22分かかりました（252GB、$8.36/時間）。ステージングでは、重みをRunPodネットワークボリュームに一度だけ配置します。

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- ボリュームは、ポッドが実行されているかどうかにかかわらず、毎月課金されます（以前のバージョンの300GBは約$21/月、$0.07/GB）。そのため、ステージングを行うのは人間だけであり、エージェントツールはできません。
- ボリュームは1つのデータセンターに存在するため、プロファイルのポッドはそこでのみ起動し、オファーと計画はその場所で価格設定されます。ネットワークストレージとプロファイルのGPUを備えたものを選択します。`offrig gpus`とRunPodのコンソールには、それらの場所が表示されます。
- ダウンロードは、そのデータセンターで利用可能な最も安価なGPUポッドで実行されます。ポッドは、成功時、失敗時、またはタイムアウト時に終了します。
- ボリュームは、ダウンロードが開始される前にプロファイルに記録されるため、ステージングが失敗した場合でも、それは決して忘れられません。再実行して再開するか、`--remove`を使用します。
- ステージングされた起動では、Hugging Faceはオフラインで実行され、ステージが完了したとき（ボリュームにマーカーが設定されたとき）にのみ実行されます。半ステージングされたボリュームは、失敗する代わりに残りの部分をダウンロードします。

## 資金の安全性

- 起動前に、offrigは最も安価な空きリソースと、ポッドが実行されている場合の残りのリソースを表示します。残りのリソースが1時間未満の場合、オーバーライドしない限り、拒否されます。残りのリソースがゼロになると、RunPodはアカウント内のすべてのポッド（offrigが管理していないものを含む）を停止します。
- 自動停止は、すべてのGPUの使用率が5%未満になった30分後にポッドを終了します（設定可能、またはオフにできます）。
- ポッドが実行されている状態でアプリケーションを閉じると、それを終了するか、実行し続けるかを尋ねられます。
- offrigは、名前を付けたポッドにのみアクセスします。CLIとアプリケーションの場合は`offrig-<profile>`、プロジェクトのサイドカーレーン（「レーン」を参照）の場合は`offrig-<tag>-<profile>`です。サイドカーは、他のレーンのポッド、プレーンレーンのポッド、またはその他のポッドには決してアクセスしません。それらはリストに表示されるだけで、変更されることはありません。
- エージェントセッションの場合、上限は支出前に適用されます。計画の最悪のケース（現在の価格×最大時間）は、人間が設定した予算と比較され、それを超えると拒否されます。起動には計画IDのみが必要なため、エージェントは独自の価格を設定できません。
- すべてのサイドカー起動には、計画の期限までにポッドを終了する監視機能があり、ランナーはキューが空になるとすぐにポッドをシャットダウンします。

## マシンに変更されるもの

| 何 | 場所 | 元に戻す |
|---|---|---|
| Zedプロバイダー `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`。最初のオリジナルは`settings.json.offrig.bak`として保持されます。 |
| Zedデフォルトモデル（要求した場合のみ） | 同じファイル | `offrig zed-remove`は、以前のデフォルトを復元します。 |
| `OFFRIG_API_KEY`（プレースホルダー。Zedはキーを必要とします） | ユーザー環境 | `setx OFFRIG_API_KEY ""`または、システムプロパティで削除します。 |
| SSHエイリアス `offrig` | `~/.ssh/config`、`# >>> offrig:offrig >>>`マーカーの間 | マークされたブロックを削除します。 |
| SSHエイリアス `offrig-<tag>`、サイドカーから起動したプロジェクトごとに1つ | `~/.ssh/config`、`# >>> offrig:offrig-<tag> >>>`マーカーの間 | マークされたブロックを削除します。 |
| プロジェクトレーン | `%APPDATA%\offrig\lanes.toml`（プロジェクトパス、タグ、エイリアス、トンネルポート） | レーンにポッドが実行されていない間に、プロジェクトのエントリを削除するか、ファイル全体を削除します。 |
| ポッドホストキー | `~/.ssh/known_hosts_offrig` | ファイルを削除します。 |
| 設定 | `%APPDATA%\offrig\config.toml` | ファイルを削除します。 |
| ステージングされた重み（`offrig stage --yes`を使用した場合のみ） | RunPodネットワークボリューム `offrig-<profile>`。毎月課金されます。 | `offrig stage <profile> --remove --yes` |

Zedの設定のコメントとレイアウトは保持されます。編集はJSONC構文ツリーを通して行われます。

## 脅威モデル

- **RunPod API key.** Read from `RUNPOD_API_KEY`; never written to disk or logs. The Zed
  provider is deliberately not named `runpod`, because Zed would then read
  `RUNPOD_API_KEY` and send it to the model server.
- **Model server.** Reachable only through SSH with your key. Password login is off on
  the pod, and sshd allows only local forwarding.
- **Host keys.** Pinned per endpoint in a separate known-hosts file. offrig forgets a key
  only when the pod endpoint changes, because RunPod reuses ip:port pairs across pods.
- **Shell injection.** Model names are validated against Ollama's name syntax before
  they reach a remote shell.
- **Orphaned tunnels.** If offrig dies, its `ssh` may keep holding the port. On the next
  start offrig kills it, but only if the listener is `ssh.exe` carrying offrig's exact
  forward spec. Anything else on the port is refused, never killed.
- **No telemetry.** offrig talks only to RunPod's API, your pod, and your local Ollama
  (to compare model lists).

## テスト

`cargo test --workspace`は250以上のテストを実行し、少なくとも90%の行をカバーします（それ以下の場合はCIが失敗します）。

- **コアライブラリ：** RunPodの解析、両方のエンジンのポッド仕様、SSH構成、Zed JSONCの編集、ガードルール、コストとアイドルロジック、ストアとその移行、ロール、コンテキストアセンブリ、決定的なチェック、ランナーの決定、監視機能、およびステージング（ステージが失敗した場合にポッドを終了させるモックRunPodを含む）。
- **アプリケーション：** 状態管理と、eguiのテストハーネスでのクリック操作によるUIテスト。
- **CLI：** 終了コード、ログレベル、およびAPIキーが出力に表示されないこと。
- **サイドカー：** stdio経由でのモックRunPodとのエンドツーエンドテスト、実際の監視プロセス、およびモックポッドモデルに対する実際のランナープロセス。すべてのツールエラーにはコードが含まれます。

`scripts/verify.sh`（または`scripts/verify.ps1`）は、フォーマットチェック、clippy、テスト、および各バイナリの簡単な実行を1つのコマンドで実行します。CIはまた、`cargo deny`（OSVスキャン）、`Cargo.lock`へのカバレッジ、および`atlas check`も実行します。

### ライブテスト記録（2026年10月2日、ミディアムティア、A100 80GB、約0.45ドル）

- ポッドは80秒程度で起動し、sshd、トンネル、およびポッドのOllama 0.35.0が応答します。
- ポッドに約150〜250MB/sの速度で97GBのモデルがダウンロードされます。
- `qwen3-coder:30b-a3b-q8_0`と`gpt-oss:120b`はそれぞれ、トンネル経由で正しいツール呼び出しを含むストリーミングチャットに応答しました。これらは、ポッドのVRAMの36GBと64GBを消費しました。ローカルのOllamaは何もロードされず、ローカルGPUにも存在しませんでした。
- CLIとアプリケーションの両方から、7つのガードチェックすべてがパスしました。
- 突然終了したCLIは、その`ssh`をポートに保持したままになりました。次の実行で、ポートが再利用されました。
- アプリケーションのトンネル、チェック、モデルテスト、およびシャットダウンは、ボタンを通じて実行されました。

ライブ実行で発見され、修正され、テストされたバグ：バックグラウンドで実行されている`&&`リストが、sshのstdoutを開いたままにして、プル開始を停止しました。ポッドリストには、GPUタイプが不足していました（`includeMachine=true`がない場合）。起動チェックでは、実行中のポッドの価格が2回カウントされました。

### サイドカーリハーサル（2026年10月3日、スモールティア、RTX 2000 Ada、0.08ドル予約）

インストールされた`offrig-mcp`が、エージェントが呼び出す方法であるstdio経由で実行されます。

- `offrig_plan`は、最悪の場合で0.5時間を0.15ドルと見積もり、`offrig_launch`がそれをコミットし、監視機能を起動し、2回目の呼び出しで同じジョブを返しました。1つのポッドが0.24ドル/時間でレンタルされました。
- レンタル後100秒でSSHが起動し、`qwen3:4b`がプルされ、150秒で準備ができました。
- `offrig_ask`は、46秒でゲームデザイナーの引き継ぎを実行しました。応答は、その受け入れチェックを満たし、メモリから5つの箇条書きの制約を維持しました。
- ポッドはインターネットにアクセスしました（Wikipedia、GitHub API）。ポッドが取得したデータに基づいて、モデルは現在の質問に正しく答えました。質問されたときに、ライブアクセスがないと答えました。
- 新しいサイドカープロセスから起動された`offrig_shutdown`が、ポッドを終了し、処理を閉じました。監視機能は、計画が終了したことを検出し、終了しました。ローカルGPUは、全体を通してアイドル状態を維持しました。

発見され、修正された点：ポッドは一度に1つのリクエストを処理しました（`OLLAMA_NUM_PARALLEL=1`）。4つのスロットで8つの並列リクエストを処理し、同じGPUで40〜102トークン/秒になりました。そのため、すべてのプロファイルには現在、`parallel = 4`があります。理由なしに`complete`が拒否されました（現在は「受け入れチェックが満たされました」がデフォルトになりました）。失敗には、依然として理由が必要です。ステータスは、セッションがアクティブな間に短い記録を記録することを示唆していました。リプライに漏洩するテキストは削除され、リプライが空になった場合、`max_tokens`を発生させるようにします。

### ランナーリハーサル（2026年10月3日、スモールティア、RTX 2000 Ada、0.04ドル予約）

5つの引き継ぎがあり、そのうちの1つは別の引き継ぎに依存しており、誰も制御していません。`offrig_run`によって実行されました。

- 4つのスロットで同時に4つの引き継ぎが実行されました（16GBのVRAMのうち12.9GB）。依存関係のある引き継ぎは、依存関係が完了した瞬間に開始され、その結果に基づいて構築されました。
- 受け入れチェックをカバーする3つの引き継ぎは、それ自体で完了しました。ライバルのバックストーリー（部分的なチェック）と、物語（チェックなし）は、レビューに送られました。
- レビューは、物語を返しました（「川はプロジェクトにちなんで名付けられました」）。ライブランナーはそれを採用し、フィードバックに基づいて修正しました（「ヴェイル川」）。
- キューは6.5分（6ターン、20,861トークン）で処理を完了し、ランナーはポッド自体をシャットダウンしました。

判明したこと：決定的なチェックは、設計の品質ではなく、構造を検証します。4Bモデルは、あいまいな動詞を使用して「3つの動詞」に合格しました。したがって、`accept_on_checks`は構造的な作業用であり、設計作業はレビューに送られます。qwen3:4bは、3行の物語でも、ターンあたり約4,000トークンを考えていました。キューは、独立した引き継ぎが十分にある場合にのみ、すべてのスロットをビジー状態に保ちます。依存関係のあるチェーンは、一度に1つずつ実行されます。

### FrontierとSGLangの実行（2026年10月3日、4.27ドル予約）

| 実行 | ポッド | 準備完了後 | キュー | 予約 |
|---|---|---|---|---|
| frontier-mini（Qwen3-Coder-30B FP8） | 1 × RTX PRO 6000 | 5.5分 | 4つの引き継ぎ、15秒 | $0.30 |
| frontier-mini-awq（Qwen3-Coder-30B AWQ） | 1 × RTX PRO 6000 | 4分 | 4つの引き継ぎ | $0.25 |
| **frontier（Qwen3-Coder-480B AWQ）** | **4 × RTX PRO 6000** | **22分**（278MB/秒で252GB、その後ロード） | **20秒で31回のハンドオフ** | **$3.59** |
| 30Bの群れによるスキャン | 1 × RTX PRO 6000 | 10分（ポッドの配置に時間がかかる） | スキャンのみ | $0.43 |

- SGLang v0.5.20（cu130）がBlackwell上で実行：flashinferアテンション、4ビットMoE重みに対しては`awq_marlin`、4つのカード上でPCIe経由のテンソル並列処理。`/dev/shm`は352GBだった。
- 最先端モデルのパフォーマンスは、小規模モデルよりも明らかに優れている：会話中の発言、および3つのテストに合格したRustモジュール（ローカルで確認）。30B FP8で実行した場合、同じタスクはコンパイルされなかった。
- 1つのロードされたモデルが、群れ全体にサービスを提供する。コピーは不要。384トークンの応答で並行スキャンを行い、1秒あたりの合計トークン数は次のとおり。

| エージェント | 4つのGPUで480B | 1つのGPUで30B AWQ |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

エージェントの数が増えると、エージェントごとの速度は低下する（480B：88 → 64で24トークン/秒）、ただし合計スループットは増加し続ける。480Bのパフォーマンス向上は、256を超えると停滞する。最先端モデルのKVキャッシュは398,526トークンを保持するため、2〜8kトークンの実際のハンドオフコンテキストを使用すると、最先端モデルは64を同時に処理し、1つのカードのSGLangモデルは32を同時に処理する。

開発中に発見され、修正された点：リビジョンがパディングされた出力で、ヘッダーチェックに合格するように修正（現在は組み込みの`no_repeats`チェック）、リビジョンがインプレースで再構築される。役割が独自の必須出力を配信物に漏洩していた（ハンドオフは現在、フォーマットを設定する）。ヘッダーフィードバックはMarkdown形式を指す。変更されていないリビジョンは、繰り返す代わりに停止する。

まだライブで検証されていない点：Zedのエージェントパネルから送信されたチャット（Zedが使用するリクエストの形状が直接テストされる）、および1.1.0の準備完了チェック、ハングレポート、およびレンタルされたポッドでの正確なCUDAピン（[ステータス](#status)を参照）。

## 標準への準拠

スタジオのワークフロー標準に基づいて評価（0件の不備、1件の部分的な準拠、2件の準拠、3件の模範的な準拠）。

- **PIN_PER_STEP：2。** ポッドイメージはバージョンタグに固定される（`ollama/ollama:0.35.0`、`lmsysorg/sglang:v0.5.20-cu130`。レシピは`latest`を拒否する）、コンパイラは1.98.1に、依存関係は`Cargo.lock`で、Atlasエンジンはフリートの1.24.0に固定される。各ハンドオフターンは、そのモデル、役割ハッシュ、およびプロンプトハッシュを記録する。モデルは、ダイジェストではなく、タグまたはリポジトリIDで固定される。
- **ANDON_AUTHORITY：3。** すべてのステップで、欠陥が発生すると実行が停止する。ディスク容量を超える重みを持つプランは、費用が発生する前に拒否される。失敗したプルは、起動を停止する。Zedによる編集で、読み戻しができない場合は、書き込まれない。破損した設定ファイルは報告され、再書き込みされることはない。CIは、fmt、clippy、テスト、ライセンス、およびアドバイザリーでブロックされる。
- **NAMED_COMPENSATORS：2。** 変更不可のアクションには、それぞれ以下のとおり、アンドゥが用意されている。
- **DECOMPOSE_BY_SECRETS：2。** 独自の理由で変更されるものごとに、1つのモジュール：RunPodのAPI（`runpod`）、ポッドの内容（`spec`）、トランスポート（`tunnel`、`remote`）、各ローカルファイルオフリグの編集（`sshconfig`、`zed`）、およびルール（`guard`、`cost`）。フロントエンドは、プレゼンテーション以上のロジックを持たない。
- **UNCERTAINTY_GATED_HUMANS：2。** オフリグは、結果がコストがかかるか、または損失が発生する場合にのみ質問する：1時間未満の実行時間で起動する場合、ポッドを終了する場合（失われるものが明記される）、ポッドがまだ課金されている状態で終了する場合。2つの決定は、人間のみが行うものであり、エージェントツールでそれらを行うことはできない：予算の上限と、月額で課金されるボリュームのステージング。コードでチェックできないハンドオフ出力は、完了する代わりにレビューで待機する。
- **EXTERNAL_VERIFIER：該当なし。** 特殊な主張はない。

**補償策**

| アクション | 元に戻す | アンドゥ後の状態 | 所有者 |
|---|---|---|---|
| ポッドを作成する（課金を開始） | `offrig down <profile> --yes`、アプリのシャットダウン、または自動停止 | ポッドが終了し、課金が停止 | オフリグを実行するオペレーター |
| ポッドを終了する | ディスクには影響しない。プロファイルとモデルを再プルする（ネットワークボリュームに保存されている）。 | 新しいポッド、同じプロファイル | オペレーター |
| Zedプロバイダーまたはデフォルトモデルを書き込む | `offrig zed-remove`、または`settings.json.offrig.bak`を復元する | オフリグの前のZed | オペレーター |
| `OFFRIG_API_KEY`を設定する | `setx OFFRIG_API_KEY ""`、またはシステムプロパティで削除する | 変数が削除される | オペレーター |
| SSHエイリアスを書き込む | `~/.ssh/config`でマークされたブロックを削除する | 以前と同じ構成 | オペレーター |
| プロジェクトレーンを割り当てる（プロジェクトの最初の`offrig_plan`） | レーンで実行されているポッドがなくなった場合、プロジェクトのエントリを`lanes.toml`から削除する。新しいプランが再度割り当てる。 | レーンは再利用可能。エイリアスブロックは別（上記の行）。 | オペレーター |
| レーンのSSHエイリアスブロックを書き込む（サイドカー起動） | `offrig_shutdown`は、プランのポッドを名前で指定するとき（または起動に失敗した場合）に削除する。それ以外の場合は、`~/.ssh/config`の`# >>> offrig:offrig-<tag> >>>`ブロックを削除する。 | 以前と同じ構成。他のレーンのブロックは変更されない。 | オペレーター |
| ポッドにモデルをプルする | `ollama rm <model>`をポッドに配置するか、ポッドを終了する | モデルが削除される | オペレーター |
| 孤立したオフリグトンネルを停止する | 不要。このレーンの正確なエイリアスとフォワードを持つ`ssh`のみが停止され、他のレーンのものは停止されない。 | ポートが解放される | オフリグ |
| サイドカー起動（`offrig_launch`） | `offrig_shutdown`。セットアップが失敗した場合は自動的に、期限が切れた場合はウォッチドッグ、キューが空になった場合はランナー。 | ポッドが終了し、測定された費用とともにプランが閉じられる | 呼び出しエージェント。ウォッチドッグがバックストップとして機能する。 |
| ボリュームをステージングする（`offrig stage --yes`、月額課金） | `offrig stage <profile> --remove --yes` | ボリュームが削除され、プロファイルがダウンロードに戻る | ステージングを行ったユーザー |
| ジョブポッドでジョブを開始する（`offrig_exec action=start`） | `offrig_exec action=stop`、または`offrig_shutdown` | ジョブが停止し、開始したすべてのものが停止する。ログはポッドが削除されるまで残る。 | 呼び出しエージェント |
| ジョブポッドで短いコマンドを実行する（`offrig_exec action=run`） | コマンド自体が行う処理のみ。最大で120秒以内に、または`timeout`または`offrig_shutdown`によって終了。 | コマンド実行後に残った状態のポッド | 呼び出しエージェント |
| ジョブポッド（`offrig_put`、`offrig_get`）との間でファイルをコピーする | コピーを削除する（ポッド上、`offrig_exec`；ここではファイル） | コピー前の状態 | 呼び出しエージェント |

## レイアウト

```text
crates/offrig-core   library: RunPod client, pod specs and engine recipes, tunnel, remote
                     ops, Zed and SSH edits, guard, cost and idle logic, session workflow,
                     project lanes, project store, roles, context assembly, checks,
                     runner decisions, watchdog, staging
crates/offrig-cli    `offrig` command line
crates/offrig-app    `offrig-app` desktop app (egui)
crates/offrig-mcp    `offrig-mcp` side-car: MCP server for agents, plus the detached
                     watchdog and runner processes
docs/                the side-car's design and its research grounding
atlas/               Atlas map of the repo (regenerate with `atlas map`)
```

## ライセンス

MIT。詳細は[LICENSE](LICENSE)を参照。

---

<a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a>によって作成。
