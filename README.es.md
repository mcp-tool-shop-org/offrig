<p align="center">
  <a href="README.ja.md">日本語</a> | <a href="README.zh.md">中文</a> | <a href="README.md">English</a> | <a href="README.fr.md">Français</a> | <a href="README.hi.md">हिन्दी</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (BR)</a>
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

Ejecute modelos grandes en GPU alquiladas de RunPod, con la garantía de que nunca se ejecutarán en su propia GPU. Una aplicación de escritorio, una CLI y un componente secundario MCP para agentes, todo ello basado en una única biblioteca de Rust.

El componente secundario permite a un agente planificar una sesión de pago dentro de un presupuesto establecido por el usuario, alquilar las GPU y entregar una cola de tareas priorizadas a un ejecutor independiente. El ejecutor mantiene ocupada cada ranura de modelo, revisa solo las comprobaciones que fallaron y apaga el pod cuando la cola está vacía. Un sistema de vigilancia termina el pod al final del plazo establecido en el plan, incluso si todo lo demás ha desaparecido.

## Estado

Probado en funcionamiento el 2026-10-02 y 2026-10-03, con un costo total de aproximadamente $5:

- **Frontier:** 4 × RTX PRO 6000 (384 GB) ejecutando Qwen3-Coder-480B (4-bit AWQ) en SGLang, listo en 22 minutos, luego 31 transferencias completadas en 20 segundos, por $3.59.
- **Datos de Swarm:** un modelo cargado sirve a cientos de agentes a la vez. El modelo de 480B alcanzó 4.059 tokens/s con 512 agentes; un modelo de 30B en una sola tarjeta alcanzó 10.147 tokens/s con 256.
- **Ejecutor:** una cola con dependencias y reenvíos de revisión, funcionó sin que nadie lo controlara, el pod se apagó al finalizar.
- **Garantía:** la GPU local permaneció inactiva durante todas las ejecuciones.

En uso diario desde el 2026-10-07 por dos proyectos a la vez, cada uno en su propio entorno: ejecuciones de entrenamiento para aspire-si en `job` pods, y renderizados de música para ai-jam-sessions en `jam` pods.

El 2026-10-11, un pod de SGLang registró `Application startup complete`, luego `/health` permaneció en estado 503 durante 7 minutos. Las GPU estuvieron cerca del 100% a aproximadamente 100 W, y `/v1/models` devolvió 200. Los pods que funcionaron durante esa semana usaron CUDA 13.0. Ese pod se colocó en CUDA 13.2. No se ha demostrado que 13.2 sea la causa; el pin permite que un perfil permanezca en la versión que funcionó. Este lanzamiento informa sobre ese problema y no considera que una receta esté lista hasta que una finalización devuelva una opción. Un perfil también puede fijar la versión de CUDA del host exactamente.

Not proven on a rented pod for this release: the one-token ready check, the
`engine_unresponsive` and `engine_hung` reports, and a create that sends
`gpu.allowedCudaVersions`. Those paths are covered by tests against a fake ssh session
and a fake RunPod. No pod was started to re-check them. RunPod's documented v1
`POST /pods` enum, read 2026-10-11, still stops at 13.0. 13.1 and 13.2 are in offrig's
list because that writer pod landed on 13.2. A floor still sends only
`gpu.minCudaVersion`.

Construido y probado, a la espera de una decisión: preparación de los pesos de frontier en un volumen de red, aproximadamente $21/mes (ver [Preparación](#staging-weights-on-a-network-volume)).

Siguiente: la primera cola de frontier real, planificada en su totalidad antes del lanzamiento; transferencias de código compiladas y probadas en el pod.

## Qué hace

Desde una sola ventana (o un solo comando), offrig:

1. muestra su saldo de RunPod, los precios de las GPU en tiempo real y cuánto tiempo durará el saldo;
2. inicia un pod para un nivel, desde 1 tarjeta pequeña en Ollama hasta 4 × RTX PRO 6000 en SGLang, y carga sus modelos en el pod;
3. abre un túnel SSH al pod;
4. agrega los modelos del pod a Zed como su propio proveedor;
5. ejecuta siete comprobaciones para asegurarse de que los modelos no se ejecuten en esta máquina;
6. apaga el pod o lo termina después de un período con todas las GPU inactivas.

A través del componente secundario, un agente también planifica sesiones dentro de un presupuesto, mantiene la memoria del proyecto entre compactaciones y reinicios, y ejecuta colas de transferencia sin supervisión (ver [El componente secundario](#the-side-car-for-agents)).

## La garantía y cómo se mantiene

- **El servidor de modelos es inalcanzable excepto a través del túnel.** El pod ejecuta un motor fijado, Ollama (`ollama/ollama:0.35.0`) o SGLang (`lmsysorg/sglang:v0.5.20-cu130`), vinculado al bucle de retorno del propio pod, y el pod expone solo `22/tcp`. No hay ningún punto final HTTP público que se pueda encontrar o explotar. Una receta no puede mover el motor fuera del bucle de retorno.
- **Zed se comunica con el túnel, en su propio puerto.** El túnel escucha en `127.0.0.1:11435`. Su Ollama local está en `11434`. offrig se niega a colocar el túnel en `11434`, por lo que un túnel inactivo no puede conectarse al servidor local: la solicitud falla en su lugar.
- **Zed nunca cambia de proveedor.** Los modelos del pod son un proveedor `offrig` separado en Zed. Si el pod está inactivo, elegir uno de ellos genera un error; Zed no intenta con otro proveedor.
- **Los pesos nunca existen localmente.** Los modelos se extraen en el pod, por el pod (o se descargan allí desde Hugging Face, o se leen desde un volumen de red preparado).

Las comprobaciones de seguridad verifican esto cada vez, a partir de hechos que offrig puede observar:

| Comprobación | Falla cuando |
|---|---|
| El túnel evita el puerto de Ollama local | el puerto del túnel es 11434 |
| Zed envía los modelos del pod a través del túnel | la URL del proveedor de Zed es cualquier cosa que no sea el túnel |
| Ollama del pod no está expuesto a Internet | el pod asigna el puerto 11434 públicamente |
| El túnel termina en el pod | la lista de modelos a través del túnel difiere de la lista leída en el pod a través de SSH |
| Los modelos del pod no están en esta máquina | un modelo del pod también existe en el Ollama local |
| Ningún modelo del pod comparte un nombre con un modelo local de Zed | un nombre en el proveedor de offrig también está en la lista de Ollama local de Zed |
| Todos los modelos que Zed ofrece están en el pod | Zed ofrece un modelo que el pod no tiene |

## Instalación

Necesita Windows con OpenSSH (integrado), Zed si desea los modelos en un editor y una cuenta de RunPod.

1. Descargue `offrig-<version>-windows-x64.zip` de
[Releases](https://github.com/mcp-tool-shop-org/offrig/releases), verifíquelo con el
`SHA256SUMS` de la versión y descomprímalo en su `PATH`. Contiene `offrig.exe` (la CLI),
`offrig-app.exe` (la aplicación) y `offrig-mcp.exe` (el componente secundario).
Para compilar desde el código fuente: `cargo build --release`, con Rust 1.98.1 (fijado en
`rust-toolchain.toml`).
2. Coloque su clave de API de RunPod en la variable de entorno del usuario `RUNPOD_API_KEY`.
3. Agregue su clave pública SSH en la configuración de la cuenta de RunPod. offrig utiliza
`~/.ssh/runpod_rustline` si está presente, luego `~/.ssh/id_ed25519`.
4. Para los agentes, registre el componente secundario con Claude Code en el ámbito del usuario:
`claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`. Esto abre el
almacén de un proyecto solo en el primer uso, por lo que es inofensivo en los proyectos que nunca lo utilizan.

El [manual](https://mcp-tool-shop-org.github.io/offrig/handbook/) explica el
primer pod, el componente secundario, la configuración, los carriles y los pods de trabajo.

## Use

**Aplicación:** inicie `offrig-app`, seleccione un perfil, presione **Iniciar pod**. Cuando esté listo, los
modelos aparecerán en el panel de agentes de Zed como "RunPod · …". Reinicie Zed una vez después del primer
inicio para que detecte `OFFRIG_API_KEY`.

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

`offrig --help` y `offrig <command> --help` muestran cada comando y opción. El bloque
anterior es el conjunto diario. `offrig init`, `offrig models`, `offrig zed` y
`offrig up --no-zed` se encuentran en el [manual](https://mcp-tool-shop-org.github.io/offrig/handbook/usage/).

### Salida, códigos de salida y errores

- **Niveles de registro:** `-q` imprime solo los errores y los resultados propios de un comando; `-v` agrega cada
llamada a RunPod y su tiempo de ejecución; `--debug` agrega los cuerpos de respuesta fallidos y las cadenas de errores completas.
La clave de API se elimina en todos los niveles.
- **Códigos de salida:** `0` éxito, `1` algo que debe corregirse en su lado (argumentos, configuración, un
control o rechazo de presupuesto, una clave faltante), `2` una falla en tiempo de ejecución (RunPod, red, ssh,
tiempo de espera, sin capacidad).
- Los **errores del componente secundario** son resultados, nunca errores de protocolo: `ok:false` con un `code` estable,
el texto `error`, un `next_action` y `retryable`. Los códigos se enumeran en la
[referencia del manual](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/).

## El componente secundario (para agentes)

`offrig-mcp` es un servidor MCP que un agente, como Claude Code, llama como un instrumento. Mantiene una base de datos por proyecto en `<project>/.offrig/offrig.db` que sobrevive a cada pod,
por lo que una sesión sobrevive a la compactación o a un reinicio sin tener que volver a explicar nada.

| Herramienta | Qué hace |
|---|---|
| `offrig_status` | El proyecto, el presupuesto, el saldo de RunPod y el tiempo restante, los pods de offrig, cada plan abierto con su `plan_id`, el nombre del carril y del pod, la cola de transferencia con el trabajo obsoleto marcado. Cada pod incluye `gpu_count` y `gpus` (`2x <type>`) del registro de RunPod, además del `planned_gpu_count` del plan. |
| `offrig_offers` | Ofertas de GPU en vivo para un recuento de GPU |
| `offrig_plan` | Estima el costo de una sesión en su peor caso (precio en vivo x horas máximas); se rechaza si excede el presupuesto restante. La respuesta indica el `ssh_alias` del carril y el `pod_name` que creará el inicio, y el `container_disk_gb` que solicitará (la opción `container_disk_gb` anula el del perfil; consulte "Disco del contenedor"). Las opciones `max_price_hr` y `no_fallback` restringen las GPU que puede alquilar (consulte "Fijar el hardware de un plan"); la opción `wait_minutes` establece cuánto tiempo se reintenta el inicio cuando no hay capacidad (consulte "Esperar a que haya capacidad"). |
| `offrig_memory_search` | Busca la memoria del proyecto activo, cada resultado con la fuente y la fecha |
| `offrig_memory_record` | Agrega una descripción breve, una restricción, una decisión, un hecho o un punto de control; los cambios son reemplazos con una razón |
| `offrig_handoffs` | Encola las transferencias dirigidas por roles (cada una necesita una verificación de aceptación; verificaciones deterministas opcionales), las enumera, muestra una vista previa de los bloques de roles, muestra la mejor salida de una transferencia (también se escribe en `.offrig/out/`), registra los resultados (completo, inválido, violación, falla, reintento con comentarios) |
| `offrig_launch` | **Gasta.** Solo toma un `plan_id`: compromete el peor de los casos, espera a que haya GPU alquilando nada, inicia el pod, abre el túnel, importa los modelos, inicia el guardián. Es idempotente por plan. Se rechaza mientras el carril ya tiene un plan o pod activo: `lane <tag> has a live pod <name> (plan <id>); shut it down first` |
| `offrig_job` | Progreso del inicio (mientras el pod se inicia, el paso se deriva del estado del pod en el momento de la llamada, y `progress.pod_status` es el estado v2 sin procesar; cada reintento de capacidad se cuenta en `progress.capacity_wait`), el tipo de GPU, `gpu_count`, `gpus`, `planned_gpu_count` y, una vez que ssh está activo, `measured_gpu_count` (una lectura sin tarjetas es `0`). `gpu_mismatch` es verdadero cuando el registro o la medición difieren del plan. La versión de CUDA del host se mide con `nvidia-smi`, con una entrada de `warnings` cuando el host es anterior a la versión mínima de CUDA del plan. Para una receta, `ready` significa que una finalización devolvió una opción. `engine_hung` y `engine_unresponsive` dejan el pod activo. Estado de actividad del guardián, minutos restantes, gasto hasta el momento |
| `offrig_ask` | Una iteración de una transferencia en el modelo del pod, el contexto se construye a partir del almacén del proyecto; la respuesta se devuelve como una salida no confiable |
| `offrig_run` | Inicia un ejecutor independiente que mantiene ocupada cada ranura de modelo: redacta cada transferencia lista, revisa como máximo dos veces contra las verificaciones fallidas, alimenta los resultados a las transferencias dependientes y luego apaga el pod cuando la cola está vacía (a menos que `keep_pod`). El trabajo que el código no puede verificar espera en la revisión |
| `offrig_put` | Copia un archivo o directorio local a un pod de trabajo (scp); las rutas de los pods relativas están debajo de `/workspace/job`. Opción `plan_id` (vea abajo) |
| `offrig_exec` | Ejecuta un comando bash en un pod de trabajo, de forma independiente para que sobreviva al componente secundario (`start`), informa si se está ejecutando o si ha terminado con su código de salida y el final del registro (`status`; `save_log` también copia todo el registro a un archivo local), lo mata (`stop`) o ejecuta un comando corto ahora y devuelve su salida estándar, error estándar y código de salida (`run`, `timeout_secs`, el valor predeterminado es 30, como máximo 120). Opción `plan_id` (vea abajo) |
| `offrig_get` | Copia un archivo o directorio desde un pod de trabajo, creando los directorios principales locales faltantes; hágalo antes del apagado, que elimina el disco del pod. Opción `plan_id` (vea abajo) |
| `offrig_shutdown` | **Destruye el pod.** Lo termina y cierra los libros del plan con el gasto medido; se rechaza mientras haya transferencias en curso, a menos que se proporcione una razón. Elimina el bloque `~/.ssh/config` del carril cuando nombra ese pod (`ssh_block_removed`) |

**¿A qué plan de trabajo aplica una herramienta de trabajo?** `offrig_put`, `offrig_exec` y `offrig_get` aceptan un `plan_id` opcional. Con exactamente un plan de trabajo abierto y sin `plan_id`, lo utilizan, como antes. Con más de un plan de trabajo abierto y sin `plan_id`, se niegan y enumeran los planes abiertos (ID, perfil, nombre del pod): nunca adivinan. Con un `plan_id`, solo actúan sobre el pod de ese plan y solo después de verificar que el nombre del pod es el que pertenece a ese plan (el otro pod de una "carril" se rechaza, no solo el de otro "carril"). Cada respuesta de la herramienta de trabajo, y `offrig_job`, indica el `project` y el `plan_id` en el que actuó (las respuestas de la herramienta de trabajo también indican el `lane`); `offrig_status` indica el `project` y enumera cada plan abierto con su `plan_id`, "carril" y nombre del pod.

Los roles provienen de Role OS (archivos y tarjetas de inicio), más cuatro roles de juego incluidos aquí en los formatos de Role OS: diseñador de juegos, diseñador de sistemas, diseñador narrativo, guardián del "lore". El límite presupuestario lo establece solo un humano:

```text
offrig budget --provider runpod 30       RunPod's cap (run in the project directory)
offrig budget --provider openrouter 8    OpenRouter's cap
offrig budget --show                     every cap, beside each provider's own balance
```

Cada inicio de "side-car" inicia un **"watchdog"**: un proceso independiente que finaliza el pod en la fecha límite del plan (tiempo comprometido + horas máximas), incluso si el agente, la sesión o el "side-car" desaparecen. Nunca actúa sobre una búsqueda fallida, se finaliza exactamente una vez, cierra los registros y escribe en `.offrig/watchdog-<plan>.log`. Si la preparación de un pod alquilado falla antes de que el motor esté en funcionamiento, el "side-car" lo finaliza en lugar de dejarlo facturando. `engine_hung` y `engine_unresponsive` son solo para informes en cada superficie: el "side-car", la CLI y la aplicación dejan el pod activo y no imprimen "listo". En la CLI y la aplicación, el comando se detiene con un error. En el "side-car", el resultado del inicio permanece con el formato de éxito, por lo que nada elimina el pod, el plan permanece comprometido y el trabajo permanece `running`. Un proceso de motor que ya se ha cerrado no es ese informe. En el "side-car", es un inicio fallido y el compensador elimina el pod. Un pod iniciado con `offrig up` o la aplicación no tiene un "watchdog". Cuando el comando se detiene con un error (un bloqueo, un motor que no responde, un motor que se cerró o un tiempo de espera), el pod sigue facturando hasta que se ejecute `offrig down <profile> --yes` or the app's Shut down. A pod RunPod reports as `ERROR` or `EXITED`: ese es el caso en el que se finaliza en cada superficie.

El diseño y sus pruebas se encuentran en [docs/sidecar-design.md](docs/sidecar-design.md).

### Índice del proyecto y verificador

```text
offrig index src docs                 chunk this project; only changed files are re-embedded
offrig index --status                 counts, model, dimension; changes nothing
offrig index --rebuild                drop every vector and embed again
offrig verify calibrate gold/grounded.jsonl gold/reasoning.jsonl --model gemma4:31b --think on
offrig verify calibrate --think-policy grounded=off,reasoning=escalate:off>on --model gemma4:31b gold/grounded.jsonl
offrig verify calibrate --report-only .offrig/out/calibrate-<model>-<time>
```

`offrig index` se integra con un Ollama solo para CPU (`embed_url`, predeterminado `http://127.0.0.1:11490`), nunca con el servidor compartido en 11434 y nunca con una GPU. Una vez que el proyecto tiene un índice, `offrig_memory_search` tiene como valor predeterminado "híbrido" (palabra clave más significado). `offrig verify calibrate` evalúa un modelo de bucle local como verificador, una afirmación "gold" por llamada, con la evidencia de esa afirmación. Se rechaza un nombre de modelo que contenga "cloud". `--num-ctx` tiene como valor predeterminado `auto` y define el tamaño de una ventana para la ejecución. `--keep-thinking` escribe el texto de "pensamiento" en `thinking.jsonl` solo en el directorio de ejecución. La [referencia del manual](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/) enumera las opciones.

### "Carriles": un "side-car" por proyecto, sin colisiones

Dos proyectos pueden ejecutar "side-cars" al mismo tiempo en una cuenta de RunPod. Cada proyecto obtiene su propio **"carril"**: un alias SSH, un puerto de túnel y una etiqueta de nombre de pod que ningún otro proyecto comparte.

| | "Carril" simple (la CLI, la aplicación, Zed) | "Carril" de un proyecto |
|---|---|---|
| Alias SSH | `offrig` | `offrig-<tag>` |
| Puerto de túnel | `11435` (ejecutor `11436`) | el primero libre de `11500`, `11502`, ... (ejecutor: el puerto anterior) |
| Puerto del "side-car" (controlador de shell) | ninguno | `11700` + la ranura del "carril": `11700`, `11701`, ... |
| Nombre del pod | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| Bloque SSH | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` proviene del nombre de la carpeta del proyecto (`aspire-si`, `ai-jam-sessions`), con un hash corto agregado cuando dos proyectos comparten el mismo nombre de carpeta. El "carril" de un proyecto se asigna la primera vez que planifica una sesión, se escribe en `lanes.toml` en el directorio de configuración de offrig y se guarda: el mismo proyecto obtiene el mismo "carril" después de cada reinicio. La asignación requiere un archivo de bloqueo y escribe el registro de forma atómica, por lo que dos "side-cars" que se inician juntos nunca comparten una etiqueta, un alias o un puerto. Ningún "carril" puede ser `11434` (el puerto del Ollama local): el rango comienza en `11500` y se rechaza un registro modificado para indicar lo contrario. Un plan registra su "carril", y su inicio, ejecutor, "watchdog" y cierre utilizan ese "carril", no la configuración global.

Un "side-car" solo coincide, enumera o detiene los pods con el nombre de su propio "carril". El "carril" de otro proyecto, los pods del "carril" simple `offrig-<profile>` y cualquier otro pod en la cuenta se dejan solos: la verificación de inicio de un pod activo solo pregunta por su propio "carril", el cierre rechaza un pod cuyo nombre no es el del "carril" del plan y la verificación de "huérfanos" del túnel elimina un `ssh` obsoleto solo cuando su reenvío y su alias son los del "carril". Los planes creados antes de que existieran los "carriles" no tienen un "carril" registrado y siguen ejecutándose en el "carril" simple, por lo que un pod iniciado bajo el esquema anterior es cerrado por el mismo plan que lo inició.

**El puerto propio del "side-car".** `offrig-mcp` utiliza MCP a través de stdio. Un controlador de shell que mantiene uno abierto para toda una sesión (cuando la propia conexión MCP de la sesión está inactiva) lo coloca detrás de un puerto HTTP de bucle, y ese puerto solía ser un número para toda la máquina (`11439`): un segundo controlador de proyecto o cualquier otro programa podría tomarlo y el primer "side-car" se quedaría sin funcionar sin decir una palabra. El valor predeterminado ahora es por proyecto, desde el "carril" del proyecto, de la misma manera que el puerto del túnel: la ranura del "carril" `i` (puerto del túnel `11500 + 2i`) obtiene el puerto del "side-car" `11700 + i`. El rango `11700` a `11763` está por encima de todos los puertos de túnel y ejecutor que puede tener un "carril" (`11500` a `11627`), el `11435` y el `11436` del "carril" simple y el `11434` del Ollama local, por lo que un puerto de "side-car" nunca puede ser un puerto de túnel. No se almacena nada nuevo: `lanes.toml` no cambia y el puerto se deriva del "carril". `OFFRIG_SIDECAR_PORT` sigue anulándolo; se rechaza un valor que no sea un puerto, que esté por debajo de 1024 o que sea `11434`, `11435`, `11436` o cualquier cosa en el rango del túnel del "carril".

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

Con `--check`, un puerto ocupado es un error que indica el puerto y, cuando un servicio secundario
(side-car) responde allí, el proyecto al que sirve: `el puerto del servicio secundario 11700 está ocupado: ya hay un servicio secundario sirviendo el proyecto <path> allí. Primero, deténgalo, o establezca OFFRIG_SIDECAR_PORT en un puerto libre`. La comprobación pregunta cómo responde el controlador (una solicitud como un proyecto que nadie está sirviendo, lo que el controlador rechaza antes de tocar cualquier herramienta), por lo que no cambia nada en un servicio secundario en ejecución. `offrig_status` informa del `sidecar_port` del carril.

**Un carril, un pod activo.** Un carril tiene un alias SSH y un puerto de túnel, por lo que sirve a un pod a la vez: un segundo pod en el carril (`offrig-<tag>-job` junto a `offrig-<tag>-jam`) volvería a apuntar el alias a sí mismo y enviaría el `offrig_put`, `offrig_exec` y `offrig_get` del primer plan a la máquina incorrecta. `offrig_launch`, por lo tanto, se niega mientras el carril tenga un plan abierto o cualquier pod activo que posea, con `el carril <tag> tiene un pod activo <name> (plan <id>); primero, ciérralo`, before anything is committed or rented. The plain lane's ` y luego inicia el servicio secundario`. La aplicación también se niega de la misma manera para un pod de otro perfil (el pod del mismo perfil aún se reutiliza). Cierre el primer plan y luego inicie el siguiente.

## Niveles

Los perfiles se almacenan en `%APPDATA%\offrig\config.toml` (se escriben en el primer cambio). Valores predeterminados:

| Perfil | GPU | Modelos | Costo típico |
|---|---|---|---|
| pequeño | 1 × RTX 2000 Ada / clase A4000 | `qwen3:4b` | aproximadamente $0.25/hora |
| mediano | 1 × RTX PRO 6000 (96 GB); A100 u H100 de 80 GB si no hay ninguna disponible | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | $2.09/hora (A100 de respaldo $1.59) |
| de vanguardia | 4 × RTX PRO 6000 (384 GB), **SGLang** | Qwen3-Coder-480B AWQ de 4 bits (252 GB), aproximadamente 130 GB restantes para el contexto | $8.36/hora |
| de vanguardia-mini | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B FP8 (31 GB): la ruta del motor de vanguardia, ensayada de forma económica | aproximadamente $1.7/hora |
| de vanguardia-mini-awq | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B AWQ (17 GB): los núcleos MoE de 4 bits de vanguardia, ensayados de forma económica | aproximadamente $1.7/hora |
| trabajo | 1 × RTX PRO 6000 (96 GB); A100 u H100 de 80 GB si no hay ninguna disponible | ninguno: un **pod de trabajo** ejecuta su trabajo, no un servidor de modelos | $2.09/hora (A100 de respaldo $1.59) |
| jam | 1 × A40 (48 GB) primero; A6000, A5000, 3090, L4 o 4090 si no hay ninguna disponible | ninguno: un **pod de trabajo** para las representaciones de canto de las sesiones de ai-jam (SoulX-Singer) | $0.49/hora (A40) |

A profile with a `recipe` runs another engine than Ollama: a pinned image
(`lmsysorg/sglang:v0.5.20-cu130`), a Hugging Face model it downloads at start, and
extra server arguments. offrig sets tensor parallelism from the GPU count, the context
length from the profile, and keeps the engine on the pod's loopback; a recipe cannot
override those. For a gated repo, `hf_token_secret` names a RunPod secret, referenced as
`{{ RUNPOD_SECRET_<name> }}` so the token never enters the pod spec. The launch reports
weights on disk while the engine downloads them, and stops at once (with the engine's
log) if the engine exits. It does not call the engine ready when `/health` answers, and
`/v1/models` answering is not readiness either. After `/health` returns 200, offrig
sends one chat completion (`max_tokens` 1, `temperature` 0) and waits up to 30 seconds.
The step is `ready` only when that reply has a choice. If the completion fails or times
out, offrig tries once more after 15 seconds. A second failure is the step
`engine_unresponsive`: the job stays running, and the error and the engine log are on
the job. The next action is `the engine is not answering completions: shut the pod down,
or wait if it is still warming up`. offrig does not shut the pod down for this, on the
side-car, the CLI, or the app. A slow first request (graph capture, JIT) is not a reason
to throw away the download and the load; the plan's deadline still bounds the cost.
The hang clock starts from `grep -c` of `Application startup complete` on the same ssh
read as the log tail, over the whole engine log, not only the last 40 lines. A chatty
engine can push the startup line out of that tail. The count still starts the clock.
If that count is at least one and `/health` then stays down for 3 minutes, the step is
`engine_hung`: those last 40 lines, the seconds since the count was first seen, and the
next action `shut the pod down: the engine finished startup but never became healthy`.
offrig reports that hang and leaves the pod up. Shutting it down stays with you, the
same as a GPU-count mismatch. A GPU-count warning is not prefixed onto that next
action, or onto the unresponsive one, so the sentence stays exact. An engine process
that has already exited is not either report. On the side-car the compensator deletes
that pod. On the CLI and the app the command stops with the error and does not delete
it.

Cada perfil enumera los tipos de GPU en orden de prioridad. v2 coloca un tipo y no realiza una copia de seguridad, por lo que offrig intenta esa lista en sí misma, en orden y nunca más allá del precio del plan. Cuando ninguno está disponible, un perfil puede esperar (`wait_for_gpu_minutes`; de vanguardia espera hasta 120 minutos): offrig comprueba cada minuto y crea el pod en el momento en que las GPU se liberan. No se alquila nada mientras espera, Ctrl+C o la opción Cancelar de la aplicación lo detienen, y si el catálogo activo no está disponible, simplemente vuelve a intentar la creación cada minuto. Los grandes conjuntos multi-GPU aparecen y desaparecen en cuestión de minutos. Los precios son los precios de la lista segura de la nube de v2 (`GET /catalog/gpus`), por GPU, multiplicados por el recuento de GPU. Una lista de NINGUNO no es gratuita. La página de precios pública no es el precio disponible. `CUDA_VERSIONS` es solo la copia de seguridad cuando falta la lista de `cudaVersions` de una GPU.

### Esperando capacidad

Un plan con `no_fallback` o `max_price_hr` a menudo no tiene suficiente capacidad, por lo que el lanzamiento intenta repetidamente en lugar de fallar, sin alquilar nada mientras lo hace. El tiempo que espera es, en orden: el `wait_minutes` del plan (un argumento `offrig_plan`, almacenado con el plan; `0` falla de inmediato), o, en su defecto, el `wait_for_gpu_minutes` del perfil. El perfil `job` tiene un valor predeterminado de 20 minutos. El tiempo de espera se reduce al tiempo que queda en el plan menos una reserva de cinco minutos, por lo que nunca se excede el plazo del plan, y, como no se alquila nada mientras se espera, no se añade nada al peor de los casos previsto. `offrig_launch` informa `capacity_wait_minutes`; mientras se espera, `offrig_job` muestra `progress.capacity_wait` (`checks`, `waited_secs`, `limit_secs`) y un paso que indica de qué comprobación se trata. Cuando el tiempo de espera se agota, el lanzamiento falla con `no capacity` y no se alquila nada.

### Asignar el hardware de un plan

Un perfil enumera los tipos de GPU en orden de prioridad y RunPod toma el primero que tenga capacidad disponible, por lo que, sin límites, un plan puede terminar utilizando una tarjeta de reserva con menos memoria, un controlador más antiguo y un precio diferente. Tres límites restringen el hardware que un plan puede utilizar. La planificación sigue siendo gratuita; los límites simplemente restringen lo que el plan puede alquilar.

| Límite | Dónde | Efecto |
|---|---|---|
| `min_cuda` | perfil (`config.toml`) | La versión CUDA más antigua del host, de la lista de RunPod (`13.2`, `13.1`, `13.0`, `12.9`, ... `11.8`). El comando de creación del pod envía ese valor mínimo como `gpu.minCudaVersion`, por lo que un host más reciente que la versión copiada más reciente también califica. Cuando el catálogo devuelve `cudaVersions` para una GPU, un plan mantiene ese tipo solo si alguna versión está disponible en o por encima de este valor mínimo. Cuando el catálogo omite la lista, la lista copiada se utiliza como valor predeterminado. Para un perfil de trabajo, se aplica la versión más reciente de este y la versión propia `[profiles.job] min_cuda` de la imagen. |
| `min_vram_gb` | perfil | La cantidad mínima total de VRAM (todas las GPU del perfil juntas) que un plan acepta. Las ofertas por debajo de este valor se descartan; también se descarta un tipo para el que RunPod no enumera ninguna memoria. |
| `max_price_hr` | argumento `offrig_plan` | El máximo que puede costar el pod, en total $/hora por todas sus GPU (la cifra que muestra `offrig_offers`). Las ofertas por encima de este valor se descartan, al igual que un tipo para el que no se indica ningún precio ahora (no se puede establecer un límite). |
| `no_fallback` | argumento `offrig_plan` | Solo se permite la primera familia de GPU del perfil. Las dos ediciones RTX PRO 6000 Blackwell (Servidor y Estación de trabajo) son una familia; todas las demás tarjetas, incluida la A100 SXM y la PCIe, son su propia familia. |

Los campos del perfil son opcionales y, por defecto, no se establecen, por lo que un `config.toml` escrito por un offrig anterior se carga sin cambios. El perfil `job` establece `min_cuda = "13.0"`.

`cuda_versions` es una lista exacta opcional además de `min_cuda`, por ejemplo, `cuda_versions = ["13.0"]`. Cuando se establece, el comando de creación del pod envía esa lista como `gpu.allowedCudaVersions`, en el orden en que se escribió, y no envía también un valor mínimo. `offrig_plan` informa de la lista. Establecerlo junto con `min_cuda`, incluida la versión propia de una imagen de trabajo, no está permitido: los dos juntos son ambiguos. Cada versión debe ser una que offrig acepte. Esa lista, de la más reciente a la más antigua, es 13.2, 13.1, 13.0 y luego 12.9 hasta 11.8. 13.2 y 13.1 son posteriores a la enumeración v1 documentada de RunPod (leída el 2026-10-11, que se detiene en 13.0). Se enumeran porque un pod de escritura el 2026-10-11 se colocó en un host 13.2. Un valor mínimo sigue enviando solo `gpu.minCudaVersion`.

`offrig_plan` calcula el peor de los casos a partir de lo que queda: `max_hours x min(max_price_hr, the dearest listed price among the remaining GPUs)`. Sin `max_price_hr`, este es el precio más alto que se indica en el perfil, como antes. Se rechaza un plan sin nada disponible, indicando el motivo de cada GPU descartada, y no se escribe nada.

El plan almacena la lista de GPU que queda, y ya sea el valor mínimo de CUDA o la lista exacta de CUDA, y `offrig_launch` solo alquila de lo que el plan almacenó, nunca de una edición posterior del perfil. `offrig_job` y el resultado del lanzamiento del archivo adjunto informan del tipo de GPU y la versión de CUDA del host que se alquiló realmente, en un objeto `rented`. La API del pod no informa de la versión de CUDA del host, por lo que, una vez que ssh está activo, el lanzamiento ejecuta `nvidia-smi` una vez a través del canal y la lee del encabezado (`CUDA Version: 12.8` o `CUDA UMD Version: 13.4` en los controladores más recientes); `rented.cuda_source` indica `nvidia-smi` o `pod API`. Si la CUDA del host es anterior al valor mínimo del plan, la GPU no es una de las que el plan enumeró, o el precio es superior al del plan, `offrig_job` devuelve una entrada `warnings` y comienza `next_action` con `WARNING`. Nada se termina automáticamente: detener el alquiler depende del llamante (`offrig_shutdown`). Si ni la API del pod ni `nvidia-smi` proporcionan una versión de CUDA, `rented.notes` lo indica y el valor mínimo no se comprueba.

### Disco del contenedor

Un pod tiene dos discos: el disco del contenedor, que es local del host, y el volumen que se monta en `/workspace`. En algunos hosts, `/workspace` es un sistema de archivos de red lento: un pod de trabajo midió 32 MB/s allí frente a 354 MB/s en su disco de contenedor, y no pudo obtener unos 130 GB de modelos a tiempo, mientras que el disco del contenedor solo tenía 60 GB. El tamaño del disco del contenedor es el `container_disk_gb` del perfil (de 30 a 60 GB en los perfiles integrados; el perfil `job` tiene 60) y se incluye en el comando de creación del pod como `disk`. `offrig_plan` toma `container_disk_gb` (de 1 a 2000) para anularlo para un plan; el plan lo almacena, el lanzamiento lo envía y el plan de respuesta y `offrig_status` muestran el tamaño en vigor.

- El disco del contenedor no tiene precio: offrig solo calcula el tiempo de GPU, por lo que el peor de los casos del plan es el mismo con cualquier tamaño. RunPod cobra por el disco; no se ha comprobado si la tarifa que informa para el pod (`offrig_job` la muestra) incluye el disco del contenedor.
- offrig no mueve sus descargas por usted. Los comandos de trabajo comienzan con `HF_HOME` en el volumen `/workspace` (`/workspace/hf`); para utilizar el disco del contenedor, establezca el suyo propio (`HF_HOME=/root/hf python ...`) en el comando.
- El disco del contenedor se elimina con el pod, como el volumen sin un volumen de red: copie los resultados con `offrig_get` antes de `offrig_shutdown`.
- El límite de 1 a 2000 es la comprobación de seguridad de offrig contra un error tipográfico; no se comprueba el límite real de RunPod.

### Pods de trabajo

Un perfil con un `job` alquila una GPU para un trabajo que se ejecuta en ella, como una ejecución de entrenamiento, en lugar de para servir un modelo. Su pod ejecuta una imagen de PyTorch asignada (`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`, CUDA 12.8 para Blackwell) con sshd y nada más:

- No sirve para ningún modelo, por lo que no hay ningún túnel y nada está conectado a Zed. sshd no permite ningún tipo de reenvío (`AllowTcpForwarding=no`); la única forma de acceder es mediante ssh al pod.
- Un perfil de trabajo no incluye ningún modelo y tampoco puede tener una receta; la comprobación de la configuración lo rechaza.
- `offrig up` y la aplicación rechazan un perfil de trabajo antes de alquilar nada. Se ejecuta a través del contenedor secundario: `offrig_plan profile=job`, `offrig_launch`, luego `offrig_put`, `offrig_exec` y `offrig_get`. El inicio está listo cuando sshd responde.
- Un comando se ejecuta de forma independiente en el pod (`setsid nohup`) en `/workspace/job`, por lo que sobrevive al contenedor secundario y a la sesión ssh. Se envía como base64, por lo que nada de su contenido es leído por el shell ssh. Su registro y estado de salida se guardan en `/workspace/offrig/jobs/`. Las descargas de Hugging Face se guardan en `/workspace/hf` en el volumen del pod.
- `offrig_exec action=run` sirve para comprobaciones rápidas (`ls`, `nvidia-smi`), no para trabajo: ejecuta el comando hasta su finalización en `timeout` (por defecto, 30 segundos, como máximo 120 segundos) y devuelve `stdout`, `stderr`, `exit_code` y `timed_out`. La salida se trunca a los últimos 64 KB de cada flujo (`truncated`) y es una salida de pod no fiable. Un comando que necesita más tiempo es un `start`.
- El registro final de un trabajo tiene barras de progreso contraídas: las actualizaciones de estilo tqdm, unidas por retornos de carro, muestran solo su último fotograma. `offrig_exec action=status save_log=<local path>` también copia todo el registro del trabajo, tal como se escribió, a un archivo local (se crean las carpetas principales), para que el registro final pueda ser corto.
- La imagen es una compilación de CUDA 12.8, por lo que un perfil de trabajo indica la versión de CUDA más antigua del host en la que se ejecuta (`min_cuda = "12.8"`) y el pod se crea con ese valor mínimo como `gpu.minCudaVersion`. Sin eso, un host con un controlador más antiguo inicia el pod y torch no encuentra ninguna GPU, después de que se haya iniciado el alquiler. El trabajo en sí puede necesitar más que la imagen: el perfil `job` también establece `min_cuda = "13.0"` en el perfil (ver arriba), porque los trabajos que ejecuta instalan un vLLM actual, cuyo PyTorch es una compilación de CUDA 13.
- El presupuesto, el plan, el sistema de vigilancia y el apagado funcionan como en cualquier otro perfil. Copie los resultados antes de `offrig_shutdown`: el disco del pod se guarda.
- `jam` es el perfil de trabajo que ai-jam-sessions utiliza para renderizar sus canciones: SoulX-Singer necesita mucho menos que una tarjeta de entrenamiento, por lo que alquila una barata de 24 a 48 GB. La configuración y la sesión se encuentran en ese repositorio (`docs/vocal-offrig.md`); offrig no sabe nada sobre cantar.

### Preparación de los pesos en un volumen de red

Un perfil de receta descarga sus pesos en cada inicio: para el modelo de vanguardia, esto tardaba unos 20 de 22 minutos en estar listo (252 GB, 8,36 $/hora). La preparación los coloca en un volumen de red de RunPod una sola vez:

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- El volumen se factura mensualmente, independientemente de si un pod se está ejecutando o no (300 GB para el modelo de vanguardia cuestan unos 21 $/mes a 0,07 $/GB), por lo que solo un humano realiza la preparación; ninguna herramienta de agente puede hacerlo.
- Un volumen se encuentra en un único centro de datos, por lo que los pods del perfil se inician solo allí, y las ofertas y los planes tienen precios en ese lugar. Elija uno con almacenamiento en red y las GPU del perfil; `offrig gpus` y la consola de RunPod muestran dónde se encuentran.
- La descarga se ejecuta en el pod de GPU más barato disponible en ese centro de datos. El pod se termina si tiene éxito, si falla o si se agota el tiempo.
- El volumen se registra en el perfil antes de que comience la descarga, por lo que una preparación fallida nunca se olvida; vuelva a ejecutar para reanudar, o `--remove`.
- Un inicio con preparación previa ejecuta Hugging Face sin conexión, solo cuando la preparación se completa (un marcador en el volumen). Un volumen con preparación parcial descarga el resto en lugar de fallar.

## Seguridad financiera

- Antes de un inicio, offrig muestra la coincidencia gratuita más barata y su margen de maniobra con el pod en ejecución. Si el margen de maniobra es inferior a una hora, se niega a menos que se anule, porque, al llegar a cero, RunPod detiene todos los pods de la cuenta, incluidos los que offrig no gestiona.
- El apagado automático termina el pod después de 30 minutos con cada GPU por debajo del 5% (configurable o desactivado).
- Cerrar la aplicación con un pod en ejecución pregunta si se debe terminar o mantenerlo en ejecución.
- offrig solo toca los pods que ha nombrado: `offrig-<profile>` para la CLI y la aplicación, `offrig-<tag>-<profile>` para el contenedor secundario de un proyecto (ver Contenedores). Un contenedor secundario nunca toca los pods de otro contenedor, los del contenedor principal o cualquier otro pod; estos se enumeran, pero nunca se modifican.
- Para las sesiones de agente, el límite se aplica antes de cualquier gasto: el peor de los casos de un plan (precio actual × horas máximas) se compromete con el presupuesto establecido por el usuario y se rechaza si se supera, y un inicio solo requiere un ID de plan, por lo que un agente no puede establecer su propio precio.
- Cada inicio de contenedor secundario tiene un sistema de vigilancia que termina el pod en la fecha límite del plan, y el ejecutor apaga el pod tan pronto como su cola esté vacía.

## Qué cambia en su máquina

| Qué | Dónde | Deshacer |
|---|---|---|
| Proveedor de Zed `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`; el original se conserva como `settings.json.offrig.bak` |
| Modelo predeterminado de Zed (solo si lo solicita) | el mismo archivo | `offrig zed-remove` restaura el valor predeterminado anterior |
| `OFFRIG_API_KEY` (marcador de posición; Zed necesita una clave) | entorno de usuario | `setx OFFRIG_API_KEY ""` o elimínelo en las propiedades del sistema |
| Alias SSH `offrig` | `~/.ssh/config`, entre los marcadores `# >>> offrig:offrig >>>` | elimine el bloque marcado |
| Alias SSH `offrig-<tag>`, uno por proyecto que se inició desde un contenedor secundario | `~/.ssh/config`, entre los marcadores `# >>> offrig:offrig-<tag> >>>` | elimine el bloque marcado |
| Canales de proyecto | `%APPDATA%\offrig\lanes.toml` (ruta del proyecto, etiqueta, alias, puerto de túnel) | elimine la entrada del proyecto mientras no haya ningún pod en ejecución en su canal, o elimine todo el archivo |
| Claves de host de pod | `~/.ssh/known_hosts_offrig` | elimine el archivo |
| Configuración | `%APPDATA%\offrig\config.toml` | elimine el archivo |
| Pesos preparados (solo con `offrig stage --yes`) | un volumen de red de RunPod `offrig-<profile>`; se factura mensualmente | `offrig stage <profile> --remove --yes` |

Los comentarios y el diseño en la configuración de Zed se conservan: las ediciones se realizan a través de un árbol de sintaxis JSONC.

## Modelo de amenazas

- **Clave de la API de RunPod.** Se lee de `RUNPOD_API_KEY`; nunca se escribe en el disco o en los registros. El proveedor Zed no se denomina deliberadamente `runpod`, porque Zed leería entonces `RUNPOD_API_KEY` y lo enviaría al servidor del modelo.
- **Servidor del modelo.** Solo se puede acceder a él a través de SSH con su clave. El inicio de sesión con contraseña está desactivado en el pod, y sshd solo permite el reenvío local.
- **Claves de host.** Se fijan por punto final en un archivo separado de known-hosts. offrig olvida una clave solo cuando cambia el punto final del pod, porque RunPod reutiliza los pares ip:puerto en todos los pods.
- **Inyección de shell.** Los nombres de los modelos se validan con la sintaxis de nombres de Ollama antes de que lleguen a un shell remoto.
- **Túneles huérfanos.** Si offrig se cierra, su `ssh` puede seguir manteniendo el puerto. En el siguiente inicio, offrig lo cierra, pero solo si el receptor está `ssh.exe` y contiene la especificación de reenvío exacta de offrig. Cualquier otra cosa en el puerto se rechaza, nunca se cierra.
- **Sin telemetría.** offrig solo se comunica con la API de RunPod, su pod y su Ollama local (para comparar las listas de modelos).

## Pruebas

`cargo test --workspace` ejecuta más de 250 pruebas, que cubren al menos el 90% de las líneas (las pruebas de CI fallan si está por debajo):

- **La biblioteca central:** análisis de RunPod, especificaciones de pod para ambos motores, configuración de SSH, ediciones JSONC de Zed, reglas de protección, lógica de costo y inactividad, el almacén y sus migraciones, roles, ensamblaje de contexto, comprobaciones deterministas, las decisiones del ejecutor, el vigilante y la preparación, incluido un RunPod simulado que demuestra que una etapa fallida finaliza su pod.
- **La aplicación:** gestión del estado más pruebas de interfaz de usuario de clic en el entorno de pruebas de egui.
- **La CLI:** códigos de salida, niveles de registro y que la clave de la API nunca aparezca en la salida.
- **El proceso secundario:** de extremo a extremo a través de stdio contra un RunPod simulado, el proceso real del vigilante y el proceso real del ejecutor contra un modelo de pod simulado; cada error de herramienta lleva un código.

`scripts/verify.sh` (o `scripts/verify.ps1`) ejecuta la comprobación de formato, clippy, las pruebas y una ejecución de prueba de cada binario en un solo comando. La CI también ejecuta `cargo deny`, un análisis OSV de `Cargo.lock`, cobertura a Codecov y `atlas check`.

### Registro de pruebas en vivo (2026-10-02, nivel medio, A100 de 80 GB, aproximadamente 0,45 $)

- El pod se inicia en aproximadamente 80 s; sshd, el túnel y el Ollama 0.35.0 del pod responden.
- Se descargan 97 GB de modelos en el pod a una velocidad de aproximadamente 150-250 MB/s.
- `qwen3-coder:30b-a3b-q8_0` y `gpt-oss:120b` respondieron a una conversación transmitida con una llamada de herramienta correcta a través del túnel. Ocuparon 36 GB y 64 GB de la VRAM del pod; el Ollama local no cargó nada y no estaba en la GPU local.
- Las siete comprobaciones de protección se superaron, tanto desde la CLI como desde la aplicación.
- Una CLI que se cerró abruptamente dejó su `ssh` manteniendo el puerto; la siguiente ejecución lo recuperó.
- El túnel, las comprobaciones, la prueba del modelo y el cierre de la aplicación se realizaron a través de sus botones.

Errores que encontró la ejecución en vivo, ahora corregidos y cubiertos: una lista de `&&` en segundo plano mantuvo abierta la salida estándar de ssh e interrumpió el inicio de la descarga; la lista de pods carecía de tipos de GPU sin `includeMachine=true`; la comprobación de inicio contó el precio de un pod en ejecución dos veces.

### Ensayo del proceso secundario (2026-10-03, nivel pequeño, RTX 2000 Ada, 0,08 $ reservados)

El `offrig-mcp` instalado se ejecuta a través de stdio, de la manera en que un agente lo llama:

- `offrig_plan` calculó 0,5 h a 0,15 $ en el peor de los casos; `offrig_launch` lo confirmó, inició el vigilante y una segunda llamada devolvió el mismo trabajo. Se alquiló un pod, a 0,24 $/h.
- SSH se activó 100 s después del alquiler, `qwen3:4b` se descargó, listo en 150 s.
- `offrig_ask` ejecutó una transferencia de diseñador de juegos en 46 s; la respuesta cumplió con su comprobación de aceptación y mantuvo la restricción de cinco puntos de la memoria.
- El pod accedió a Internet (Wikipedia, API de GitHub). Dados los datos que obtuvo el pod, el modelo respondió correctamente a las preguntas actuales; cuando se le preguntó en frío, dijo que no tenía acceso en vivo.
- `offrig_shutdown` desde un proceso secundario nuevo cerró el pod y cerró los libros; el vigilante vio que el plan se cerraba y salió. La GPU local permaneció inactiva durante todo el proceso.

Se encontraron y corrigieron: el pod atendía una solicitud a la vez (`OLLAMA_NUM_PARALLEL=1`); cuatro ranuras tardaron 8 solicitudes paralelas de 40 a 102 tok/s en la misma GPU, por lo que ahora cada perfil tiene `parallel = 4`. `complete` se rechazó sin motivo (ahora tiene como valor predeterminado "se cumplió la comprobación de aceptación"; los fallos aún necesitan uno). El estado sugería registrar brevemente mientras una sesión estaba activa. Se piensa que el texto que se filtra en una respuesta se elimina y que una respuesta vacía por pensar indica que se debe aumentar `max_tokens`.

### Ensayo del ejecutor (2026-10-03, nivel pequeño, RTX 2000 Ada, 0,04 $ reservados)

Cinco transferencias, una de las cuales depende de otra, funcionaron mediante `offrig_run` sin que nadie las dirigiera:

- Cuatro transferencias en curso a la vez en cuatro ranuras (12,9 GB de 16 GB de VRAM); la dependiente se inició en el momento en que la dependiente completó y se basó en su resultado.
- Las tres transferencias cuyas comprobaciones cubrieron la aceptación se completaron por sí solas; las historias de fondo rivales (comprobaciones parciales) y el folclore (sin comprobaciones) se enviaron para su revisión.
- La revisión devolvió el folclore ("el río lleva el nombre del proyecto"); el ejecutor en vivo lo adoptó y lo revisó en función de los comentarios ("río Veyl").
- La cola se vació en 6,5 minutos (6 turnos, 20 861 tokens); el ejecutor cerró el pod por sí solo.

Aprendido: las comprobaciones deterministas verifican la estructura, no la calidad del diseño. El modelo de 4B superó "tres verbos" con verbos débiles, por lo que `accept_on_checks` es para el trabajo estructural y el trabajo de diseño se envía a revisión. qwen3:4b gastó aproximadamente 4000 tokens pensando por turno, incluso en tres líneas de folclore. Una cola mantiene cada ranura ocupada solo cuando contiene suficientes transferencias independientes; una cadena de dependencias se ejecuta una a la vez.

### Ejecuciones de Frontier y SGLang (2026-10-03, 4,27 $ reservados)

| Ejecutar | Pod | Listo después de | Cola | Reservado |
|---|---|---|---|---|
| frontier-mini (Qwen3-Coder-30B FP8) | 1 × RTX PRO 6000 | 5,5 min | 4 transferencias en 15 s | $0.30 |
| frontier-mini-awq (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 min | 4 transferencias | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 min** (252 GB a 278 MB/s, luego carga) | **31 transferencias en 20 s** | **$3.59** |
| 30B, exploración de enjambre | 1 × RTX PRO 6000 | 10 min (colocación lenta de pods) | solo exploración | $0.43 |

- SGLang v0.5.20 (cu130) se ejecuta en Blackwell: atención flashinfer, `awq_marlin` para los
pesos MoE de 4 bits, paralelismo de tensores a través de PCIe en cuatro tarjetas; `/dev/shm` era de 352 GB.
- El trabajo del modelo más avanzado fue claramente mejor que el de los modelos pequeños: expresiones dentro del entorno, y un
módulo Rust que se compiló y superó sus tres pruebas (verificado localmente). La versión de la misma tarea del modelo 30B FP8
no se compiló.
- Un modelo cargado sirve a todo un enjambre; no se necesitan copias. Exploración de concurrencia con
respuestas de 384 tokens, tokens totales por segundo:

| Agentes | 480B en 4 GPU | 30B AWQ en 1 GPU |
|---:|---:|---:|
| 1 | 88 | 118 |
| 8 | 394 | 809 |
| 16 | 658 | 1,692 |
| 32 | 990 | 2,525 |
| 64 | 1,521 | 4,088 |
| 128 | 2,289 | 6,762 |
| 256 | 3,208 | 10,147 |
| 512 | 4,059 | — |

La velocidad por agente disminuye a medida que se agregan agentes (480B: 88 → 24 tok/s a 64), pero el
rendimiento total sigue aumentando; las ganancias del 480B se estabilizan después de 256. La caché KV del modelo más avanzado
contiene 398.526 tokens, por lo que, con contextos de transferencia reales de 2 a 8 mil tokens, el modelo más avanzado
ahora ejecuta 64 en paralelo y los modelos SGLang de una sola tarjeta, 32.

Se encontraron y corrigieron en el camino: las revisiones rellenaron la salida para pasar una verificación de encabezado (ahora una
verificación integrada `no_repeats`, y las revisiones reestructuran en el lugar); la salida requerida de un rol se filtró en los resultados (la transferencia ahora establece el formato); la retroalimentación del encabezado
indica el formato Markdown; una revisión sin cambios se detiene en lugar de repetirse.

Aún no se ha verificado en vivo: un chat
enviado desde el panel del agente de Zed (la forma de solicitud que utiliza Zed se prueba directamente), y la verificación de preparación 1.1.0, el informe de bloqueo y el pin CUDA exacto en un pod alquilado (ver
[Estado](#estado)).

## Cumplimiento de estándares

Se evaluó en función de los estándares de flujo de trabajo del estudio (0 faltantes, 1 parcial, 2 presentes,
3 ejemplares).

- **PIN_PER_STEP: 2.** Las imágenes de los pods se fijan a las etiquetas de versión (`ollama/ollama:0.35.0`,
`lmsysorg/sglang:v0.5.20-cu130`; una receta rechaza `latest`), el compilador a 1.98.1,
las dependencias por `Cargo.lock` y el motor Atlas a la versión 1.24.0 de la flota. Cada turno de transferencia registra su modelo, hash de rol y hash de solicitud. Los modelos se fijan por etiqueta o ID de repositorio,
no por resumen.
- **ANDON_AUTHORITY: 3.** Cada paso detiene la ejecución en caso de un defecto: un plan cuyos pesos
exceden el disco se rechaza antes de cualquier gasto; un error de extracción detiene el inicio; una edición de Zed
que no se puede volver a leer no se escribe; se informa un archivo de configuración dañado, nunca
se vuelve a escribir; CI bloquea en fmt, clippy, pruebas, licencias y avisos.
- **NAMED_COMPENSATORS: 2.** Cada acción irreversible tiene una anulación, que se enumera a continuación.
- **DECOMPOSE_BY_SECRETS: 2.** Un módulo por cada cosa que cambia por sus propias razones:
la API de RunPod (`runpod`), el contenido del pod (`spec`), el transporte (`tunnel`,
`remote`), cada archivo local que edita offrig (`sshconfig`, `zed`) y las reglas (`guard`,
`cost`). Los front-ends no contienen lógica más allá de la presentación.
- **UNCERTAINTY_GATED_HUMANS: 2.** offrig pregunta solo cuando el resultado es costoso o
con pérdida: inicio con menos de una hora de tiempo de ejecución, terminación de un pod (indicando lo que se pierde) y
salida con un pod que aún está facturando. Dos decisiones pertenecen únicamente a un humano, y ninguna
herramienta de agente puede tomarlas: el límite presupuestario y la preparación de un volumen, que se factura mensualmente.
La salida de la transferencia que el código no puede verificar espera en la revisión en lugar de completarse.
- **EXTERNAL_VERIFIER: n/a.** No hay afirmaciones especializadas.

**Compensadores**

| Acción | Deshacer | Estado después de la anulación | Propietario |
|---|---|---|---|
| Crear un pod (comienza la facturación) | `offrig down <profile> --yes`, la aplicación se apaga o se detiene automáticamente | pod terminado, facturación detenida | el operador que ejecuta offrig |
| Terminar un pod | ninguno para su disco; volver a lanzar el perfil y los modelos se vuelven a extraer (un volumen de red los mantiene) | nuevo pod, mismo perfil | el operador |
| Escribir el proveedor de Zed o el modelo predeterminado | `offrig zed-remove`, o restaurar `settings.json.offrig.bak` | Zed como antes, offrig | el operador |
| Establecer `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` o eliminarlo en las propiedades del sistema | variable eliminada | el operador |
| Escribir el alias SSH | eliminar el bloque marcado en `~/.ssh/config` | configuración como antes | el operador |
| Asignar una pista de proyecto (la primera `offrig_plan` del proyecto) | eliminar la entrada del proyecto de `lanes.toml` una vez que no haya pods en ejecución en su pista; un nuevo plan se asigna nuevamente | pista libre para su reutilización; el bloque de alias es independiente (fila superior) | el operador |
| Escribir un bloque de alias SSH de una pista (lanzamiento de un side-car) | `offrig_shutdown` lo elimina cuando nombra el pod del plan (también después de un lanzamiento fallido); de lo contrario, eliminar el bloque `# >>> offrig:offrig-<tag> >>>` en `~/.ssh/config` | configuración como antes; los bloques de otras pistas no se ven afectados | el operador |
| Extraer un modelo en el pod | `ollama rm <model>` en el pod, o terminar el pod | modelo eliminado | el operador |
| Eliminar un túnel offrig huérfano | no es necesario; solo se elimina un `ssh` con el alias y el reenvío exactos de esta pista, nunca el de otra pista | puerto libre | offrig |
| Lanzamiento de side-car (`offrig_launch`) | `offrig_shutdown`; automático si la configuración falla; el watchdog al final del plazo; el ejecutor cuando su cola se vacía | pod terminado, plan cerrado con el gasto medido | el agente que lo llama, con el watchdog como respaldo |
| Preparar un volumen (`offrig stage --yes`, se factura mensualmente) | `offrig stage <profile> --remove --yes` | volumen eliminado, perfil vuelve a la descarga | el humano que lo preparó |
| Iniciar un trabajo en un pod de trabajo (`offrig_exec action=start`) | `offrig_exec action=stop`, o `offrig_shutdown` | trabajo terminado con todo lo que inició; su registro permanece hasta que el pod desaparece | el agente que lo llama |
| Ejecutar un comando corto en un pod de trabajo (`offrig_exec action=run`) | solo lo que hace el comando en sí; finaliza en un máximo de 120 segundos (`timeout`) o mediante `offrig_shutdown` | el pod tal como lo dejó el comando | el agente que lo llama |
| Copiar archivos hacia o desde un pod de trabajo (`offrig_put`, `offrig_get`) | eliminar la copia (en el pod, `offrig_exec`; aquí, el archivo) | como antes de la copia | el agente que lo llama |

## Diseño

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

## Licencia

MIT. Consulte [LICENSE](LICENSE).

---

Creado por <a href="https://mcp-tool-shop.github.io/">MCP Tool Shop</a>
