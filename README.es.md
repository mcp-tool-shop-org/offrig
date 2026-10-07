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

Ejecute modelos grandes en GPU alquiladas de RunPod, con la garantía de que nunca se ejecutarán en su propia GPU. Una aplicación de escritorio, una CLI y un componente secundario MCP para agentes, todo ello basado en una única biblioteca Rust.

El componente secundario permite a un agente planificar una sesión de pago dentro de un presupuesto establecido por el usuario, alquilar las GPU y entregar una cola de tareas priorizadas a un ejecutor independiente. El ejecutor mantiene ocupada cada ranura de modelo, revisa solo las comprobaciones que fallaron y apaga el pod cuando la cola está vacía. Un sistema de vigilancia termina el pod al alcanzar el plazo establecido en el plan, incluso si todo lo demás ha desaparecido.

## Estado

Probado en funcionamiento el 2026-10-02 y 2026-10-03, con un costo total de aproximadamente 5 dólares:

- **Frontier:** 4 × RTX PRO 6000 (384 GB) ejecutando Qwen3-Coder-480B (4-bit AWQ) en SGLang, listo en 22 minutos, luego 31 transferencias completadas en 20 segundos, por 3,59 dólares.
- **Datos de Swarm:** un modelo cargado sirve a cientos de agentes simultáneamente. El modelo de 480B alcanzó 4.059 tokens/s con 512 agentes; un modelo de 30B en una sola tarjeta alcanzó 10.147 tokens/s con 256.
- **Ejecutor:** una cola con dependencias y reenvíos de revisión, funcionó sin que nadie lo controlara, el pod se apagó al finalizar.
- **Garantía:** la GPU local permaneció inactiva durante todas las ejecuciones.

En uso diario desde el 2026-10-07 por dos proyectos simultáneamente, cada uno en su propio entorno: ejecuciones de entrenamiento para aspire-si en `job` pods, y renderizados de música para ai-jam-sessions en `jam` pods.

Construido y probado, a la espera de una decisión: preparación de los pesos de frontier en un volumen de red, aproximadamente 21 dólares al mes (ver [Preparación](#staging-weights-on-a-network-volume)).

Próximo: la primera cola de frontier real, planificada en su totalidad antes del lanzamiento; transferencias de código compiladas y probadas en el pod.

## Qué hace

Desde una sola ventana (o un solo comando), offrig:

1. muestra su saldo de RunPod, los precios de las GPU en tiempo real y cuánto tiempo durará el saldo;
2. inicia un pod para un nivel, desde 1 tarjeta pequeña en Ollama hasta 4 × RTX PRO 6000 en SGLang, y carga sus modelos en el pod;
3. abre un túnel SSH al pod;
4. agrega los modelos del pod a Zed como su propio proveedor;
5. ejecuta siete comprobaciones para garantizar que los modelos no se ejecuten en esta máquina;
6. apaga el pod o lo termina después de un período de tiempo con todas las GPU inactivas.

A través del componente secundario, un agente también planifica sesiones dentro de un presupuesto, mantiene la memoria del proyecto entre las operaciones de compactación y los reinicios, y ejecuta colas de transferencia sin supervisión (ver [El componente secundario](#the-side-car-for-agents)).

## La garantía y cómo se mantiene

- **El servidor de modelos es inaccesible excepto a través del túnel.** El pod ejecuta un motor anclado, Ollama (`ollama/ollama:0.35.0`) o SGLang (`lmsysorg/sglang:v0.5.20-cu130`), vinculado al bucle de retorno del propio pod, y el pod expone solo `22/tcp`. No hay ningún punto final HTTP público que se pueda encontrar o explotar. Una receta no puede mover el motor fuera del bucle de retorno.
- **Zed se comunica con el túnel, en su propio puerto.** El túnel escucha en `127.0.0.1:11435`. Su Ollama local está en `11434`. offrig se niega a colocar el túnel en `11434`, por lo que un túnel inactivo no puede conectarse al servidor local: la solicitud falla en su lugar.
- **Zed nunca cambia de proveedor.** Los modelos del pod son un proveedor `offrig` separado en Zed. Si el pod está inactivo, seleccionar uno de ellos genera un error; Zed no intenta con otro proveedor.
- **Los pesos nunca existen localmente.** Los modelos se extraen en el pod, por el pod (o se descargan allí desde Hugging Face, o se leen desde un volumen de red preparado).

Las comprobaciones de seguridad verifican esto cada vez, a partir de hechos que offrig puede observar:

| Comprobación | Falla cuando |
|---|---|
| El túnel evita el puerto de Ollama local | el puerto del túnel es 11434 |
| Zed envía los modelos del pod a través del túnel | la URL del proveedor de Zed es cualquier cosa que no sea el túnel |
| Ollama del pod no está expuesto a Internet | el pod asigna el puerto 11434 públicamente |
| El túnel termina en el pod | la lista de modelos a través del túnel difiere de la lista que se lee en el pod a través de SSH |
| Los modelos del pod no están en esta máquina | un modelo del pod también existe en el Ollama local |
| Ningún modelo del pod comparte un nombre con un modelo local de Zed | un nombre en el proveedor de offrig también está en la lista de Ollama local de Zed |
| Todos los modelos que Zed ofrece están en el pod | Zed ofrece un modelo que el pod no tiene |

## Instalación

Requiere Windows con OpenSSH (integrado), Zed si desea los modelos en un editor y una cuenta de RunPod.

1. Descargue `offrig-<version>-windows-x64.zip` desde [Releases](https://github.com/mcp-tool-shop-org/offrig/releases), verifíquelo con el `SHA256SUMS` de la versión y descomprímalo en su `PATH`. Contiene `offrig.exe` (la CLI), `offrig-app.exe` (la aplicación) y `offrig-mcp.exe` (el componente secundario). Para construir desde el código fuente en su lugar: `cargo build --release`, con Rust 1.98.1 (anclado en `rust-toolchain.toml`).
2. Coloque su clave de API de RunPod en la variable de entorno del usuario `RUNPOD_API_KEY`.
3. Agregue su clave pública SSH en la configuración de la cuenta de RunPod. offrig utiliza `~/.ssh/runpod_rustline` si está presente, luego `~/.ssh/id_ed25519`.
4. Para los agentes, registre el componente secundario con Claude Code en el ámbito del usuario: `claude mcp add --scope user offrig -- <path>\offrig-mcp.exe`. Abre el almacén de un proyecto solo en el primer uso, por lo que es inofensivo en los proyectos que nunca lo utilizan.

El [manual](https://mcp-tool-shop-org.github.io/offrig/handbook/) guía a través de un primer pod, el componente secundario, la configuración, los entornos y los pods de trabajo.

## Uso

**Aplicación:** inicie `offrig-app`, elija un perfil y presione **Iniciar pod**. Cuando esté listo, los modelos aparecerán en el panel de agentes de Zed como "RunPod · …". Reinicie Zed una vez después del primer lanzamiento para que vea `OFFRIG_API_KEY`.

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
offrig stage frontier --dc EUR-IS-1 --yes   stage weights on a network volume (bills monthly)
```

### Salida, códigos de salida y errores

- **Niveles de registro:** `-q` imprime solo errores y los resultados propios de un comando; `-v` agrega cada llamada a RunPod y su tiempo de ejecución; `--debug` agrega los cuerpos de respuesta fallidos y las cadenas de errores completas. La clave de la API se elimina en cada nivel.
- **Códigos de salida:** `0` éxito, `1` algo que corregir en su lado (argumentos, configuración, un rechazo de protección o presupuesto, una clave faltante), `2` una falla en tiempo de ejecución (RunPod, red, ssh, tiempo de espera, sin capacidad).
- Los **errores del side-car** son resultados, nunca errores de protocolo: `ok:false` con un `code` estable, el texto `error`, un `next_action` y `retryable`. Los códigos se enumeran en la [referencia del manual](https://mcp-tool-shop-org.github.io/offrig/handbook/reference/).

## El side-car (para agentes)

`offrig-mcp` es un servidor MCP que un agente, como Claude Code, utiliza como herramienta. Mantiene una base de datos por proyecto en `<project>/.offrig/offrig.db` que sobrevive a cada pod, por lo que una sesión sobrevive a la compactación o a un reinicio sin tener que volver a explicar nada.

| Herramienta | Qué hace |
|---|---|
| `offrig_status` | El proyecto, el presupuesto, el saldo de RunPod y el tiempo disponible, los pods de offrig, cada plan abierto con su `plan_id`, carril y nombre de pod, la cola de transferencia con el trabajo obsoleto marcado. |
| `offrig_offers` | Ofertas de GPU en vivo para un recuento de GPU |
| `offrig_plan` | Establece el precio de una sesión en su peor caso (precio en vivo x horas máximas); se rechaza si excede el presupuesto restante. La respuesta indica el `ssh_alias` del carril y el `pod_name` que se creará al iniciar, y el `container_disk_gb` que solicitará (el `container_disk_gb` opcional anula el del perfil; consulte "Disco del contenedor"). El `max_price_hr` y el `no_fallback` opcionales limitan las GPU que puede alquilar (consulte "Fijar el hardware de un plan"); el `wait_minutes` opcional establece cuánto tiempo se reintenta el inicio cuando no hay capacidad (consulte "Esperar a que haya capacidad"). |
| `offrig_memory_search` | Busca la memoria activa del proyecto, cada resultado con la fuente y la fecha. |
| `offrig_memory_record` | Agrega una descripción breve, una restricción, una decisión, un hecho o un punto de control; los cambios son reemplazos con una razón. |
| `offrig_handoffs` | Encola las transferencias iniciadas por roles (cada una necesita una verificación de aceptación; verificaciones deterministas opcionales), las enumera, muestra una vista previa de los bloques de roles, muestra la mejor salida de una transferencia (también se escribe en `.offrig/out/`), registra los resultados (completo, inválido, violación, falla, reintento con comentarios). |
| `offrig_launch` | **Gasta.** Solo acepta un `plan_id`: compromete el peor de los casos, espera a que haya GPU alquilando nada, inicia el pod, abre el túnel, extrae los modelos, inicia el watchdog. Es idempotente por plan. Se rechaza mientras el carril ya tiene un plan o pod activo: `lane <tag> has a live pod <name> (plan <id>); shut it down first`. |
| `offrig_job` | Progreso del inicio (mientras el pod se inicia, el paso se deriva del estado del pod en el momento de la llamada; cada reintento de capacidad se cuenta en `progress.capacity_wait`), el tipo de GPU y la versión de CUDA del host realmente alquilados (medidos con `nvidia-smi` en el pod, con una entrada `warnings` audible cuando el host es anterior a la versión mínima de CUDA del plan), el watchdog activo, los minutos restantes, el gasto hasta el momento. |
| `offrig_ask` | Una iteración de una transferencia en el modelo del pod, el contexto se construye a partir del almacén del proyecto; la respuesta se devuelve como una salida no confiable. |
| `offrig_run` | Inicia un ejecutor independiente que mantiene ocupada cada ranura de modelo: redacta cada transferencia lista, revisa como máximo dos veces contra las verificaciones fallidas, alimenta los resultados a las transferencias dependientes y luego apaga el pod cuando la cola está vacía (a menos que `keep_pod`). El trabajo que el código no puede verificar espera en la revisión. |
| `offrig_put` | Copia un archivo o directorio local a un pod de trabajo (scp); las rutas relativas del pod están debajo de `/workspace/job`. `plan_id` opcional (vea abajo). |
| `offrig_exec` | Ejecuta un comando bash en un pod de trabajo, de forma independiente para que sobreviva al side-car (`start`), informa si se está ejecutando o si ha terminado con su código de salida y el final del registro (`status`; `save_log` también copia todo el registro a un archivo local), lo mata (`stop`) o ejecuta un comando corto ahora y devuelve su salida estándar, error estándar y código de salida (`run`, `timeout_secs`, el valor predeterminado es 30, como máximo 120). `plan_id` opcional (vea abajo). |
| `offrig_get` | Copia un archivo o directorio desde un pod de trabajo, creando las carpetas principales locales faltantes; hágalo antes del apagado, que elimina el disco del pod. `plan_id` opcional (vea abajo). |
| `offrig_shutdown` | **Destruye el pod.** Lo termina y cierra los libros del plan con el gasto medido; se rechaza mientras haya transferencias en curso, a menos que se proporcione una razón. Elimina el bloque `~/.ssh/config` del carril cuando nombra ese pod (`ssh_block_removed`). |

**En qué plan de trabajo actúa una herramienta de trabajo.** `offrig_put`, `offrig_exec` y `offrig_get` aceptan un `plan_id` opcional. Con exactamente un plan de trabajo abierto y sin `plan_id`, lo utilizan, como antes. Con más de un plan de trabajo abierto y sin `plan_id`, se rechaza y enumera los planes abiertos (id., perfil, nombre del pod); nunca adivina. Con un `plan_id`, actúa solo en el pod de ese plan y solo después de verificar que el nombre del pod es el que posee ese plan (se rechaza el otro pod de un carril, no solo el de otro carril). Cada respuesta de la herramienta de trabajo y `offrig_job` indica el `project` y el `plan_id` en el que actuó (las respuestas de la herramienta de trabajo también indican el `lane`); `offrig_status` indica el `project` y enumera cada plan abierto con su `plan_id`, carril y nombre del pod.

Los roles provienen de Role OS (dossiers y tarjetas de paquete de inicio) más cuatro roles de juego incluidos aquí en los formatos de Role OS: diseñador de juegos, diseñador de sistemas, diseñador narrativo, guardián del conocimiento. El límite del presupuesto lo establece solo un humano:

```text
offrig budget 15          set this project's cap (run in the project directory)
offrig budget             show cap, committed, spent, remaining
```

Cada inicio inicia un **watchdog**: un proceso independiente que termina el pod en la fecha límite del plan (tiempo comprometido + horas máximas), incluso si el agente, la sesión o el side-car desaparecen. Nunca actúa sobre una búsqueda fallida, se termina exactamente una vez, cierra los libros y registra en `.offrig/watchdog-<plan>.log`. Si falla la preparación de un pod alquilado, el inicio lo termina en lugar de dejarlo facturando.

El diseño y su evidencia se encuentran en [docs/sidecar-design.md](docs/sidecar-design.md).

### Carriles: un side-car por proyecto, sin colisiones

Dos proyectos pueden ejecutar side-cars al mismo tiempo en una cuenta de RunPod. Cada proyecto obtiene su propio **carril**: un alias SSH, un puerto de túnel y una etiqueta de nombre de pod que ningún otro proyecto comparte.

| | Carril simple (la CLI, la aplicación, Zed) | El carril de un proyecto |
|---|---|---|
| Alias SSH | `offrig` | `offrig-<tag>` |
| Puerto de túnel | `11435` (ejecutor `11436`) | el primero libre de `11500`, `11502`, ... (ejecutor: el puerto anterior) |
| Puerto del side-car (controlador de shell) | ninguno | `11700` + la ranura del canal: `11700`, `11701`, ... |
| Nombre del Pod | `offrig-<profile>` | `offrig-<tag>-<profile>` |
| Bloque SSH | `# >>> offrig:offrig >>>` | `# >>> offrig:offrig-<tag> >>>` |

`<tag>` proviene del nombre de la carpeta del proyecto (`aspire-si`, `ai-jam-sessions`), con un hash corto
añadido cuando dos proyectos comparten el mismo nombre de carpeta. A un proyecto se le asigna un canal la primera
vez que planifica una sesión, se escribe en `lanes.toml` en el directorio de configuración de offrig y se mantiene:
el mismo proyecto obtiene el mismo canal después de cada reinicio. La asignación utiliza un archivo de bloqueo y
escribe el registro de forma atómica, por lo que dos procesos auxiliares que se inician juntos nunca comparten una etiqueta,
alias o puerto. Ningún canal puede ser `11434` (el puerto de Ollama local): el rango comienza en
`11500`, y se rechaza cualquier registro que se modifique para indicar lo contrario. Un plan registra su canal y
su inicio, y el ejecutor, el sistema de vigilancia y el apagado utilizan todos ese canal, no la configuración global.

Un proceso auxiliar solo coincide, enumera o detiene pods con el nombre de su propio canal. El canal de otro
proyecto, los pods del canal simple `offrig-<profile>` y cualquier otro pod en la cuenta
se dejan intactos: la comprobación de inicio para un pod activo solo pregunta
por su propio canal, el apagado rechaza un pod cuyo nombre no es el del canal del plan y la comprobación de huérfanos del túnel elimina un `ssh` obsoleto solo cuando su reenvío y su alias son los del canal.
Los planes creados antes de que existieran los canales no tienen ningún canal registrado y siguen ejecutándose en el canal simple,
por lo que un pod lanzado bajo el esquema anterior se detiene mediante el mismo plan que lo inició.

**El puerto propio del proceso auxiliar.** `offrig-mcp` utiliza MCP a través de stdio. Un controlador de shell que lo mantiene
abierto durante toda una sesión (cuando la propia conexión MCP de la sesión está inactiva) lo coloca detrás de
un puerto HTTP de bucle, y ese puerto solía ser un número para toda la máquina (`11439`):
un segundo controlador de proyecto, o cualquier otro programa, podía tomarlo y el primer proceso auxiliar se quedaba
sin funcionar sin decir nada. Ahora, el valor predeterminado es por proyecto, desde el canal del proyecto, de la misma manera
que el puerto del túnel: la ranura del canal `i` (puerto del túnel `11500 + 2i`) obtiene el puerto del proceso auxiliar `11700 + i`.
El rango `11700` a `11763` se encuentra por encima de todos los puertos de túnel y ejecutor que puede tener un canal
(`11500` a `11627`), el `11435` y el `11436` del canal simple, y el de Ollama local `11434`,
por lo que un puerto de proceso auxiliar nunca puede ser un puerto de túnel. No se guarda nada nuevo: `lanes.toml` es
inalterado y el puerto se deriva del canal. `OFFRIG_SIDECAR_PORT` sigue anulándolo; un
valor que no es un puerto, que es inferior a 1024 o que es `11434`, `11435`, `11436` o cualquier cosa en el
rango del túnel del canal se rechaza.

```
offrig-mcp --sidecar-port --project <dir>           # print the port; allocates the lane if the project has none
offrig-mcp --sidecar-port --check --project <dir>   # also exit 1 if something already holds it
```

Con `--check`, un puerto ocupado es un error que indica el puerto y, cuando un proceso auxiliar de offrig
responde allí, el proyecto al que sirve: `el puerto del proceso auxiliar 11700 está ocupado: un proceso auxiliar de offrig ya está sirviendo al proyecto <ruta> allí. Primero, deténgalo o establezca OFFRIG_SIDECAR_PORT en un
puerto libre`. La comprobación pregunta de la misma manera que el controlador ya responde (una solicitud como un proyecto
que nadie está sirviendo, que el controlador rechaza antes de tocar ninguna herramienta), por lo que no cambia nada
en un proceso auxiliar en ejecución. `offrig_status` informa del `sidecar_port` del canal.

**Un canal, un pod activo.** Un canal tiene un alias SSH y un puerto de túnel, por lo que sirve a un
pod a la vez: un segundo pod en el canal (`offrig-<tag>-job` junto a `offrig-<tag>-jam`)
volvería a apuntar el alias a sí mismo y enviaría el `offrig_put`, `offrig_exec`
y `offrig_get` del primer plan a la máquina incorrecta. `offrig_launch`, por lo tanto, se niega mientras el canal tiene
un plan abierto o cualquier pod activo que le pertenezca, con `el canal <etiqueta> tiene un pod activo <nombre> (plan <id>);
deténgalo primero`, before anything is committed or rented. The plain lane's `offrig up`
y la aplicación se niega de la misma manera para un pod de otro perfil (el pod del mismo perfil sigue reutilizándose). Detenga el primer plan y, a continuación, inicie el siguiente.

## Niveles

Los perfiles se almacenan en `%APPDATA%\offrig\config.toml` (se escriben en el primer cambio). Valores predeterminados:

| Perfil | GPU | Modelos | Coste típico |
|---|---|---|---|
| pequeño | 1 × RTX 2000 Ada / A4000 | `qwen3:4b` | aproximadamente 0,25 $/hora |
| medio | 1 × RTX PRO 6000 (96 GB); A100 u H100 de 80 GB si no hay ninguna disponible | `qwen3-coder:30b-a3b-q8_0`, `gpt-oss:120b` | 2,09 $/hora (A100 de reserva 1,59 $) |
| de vanguardia | 4 × RTX PRO 6000 (384 GB), **SGLang** | Qwen3-Coder-480B AWQ de 4 bits (252 GB), aproximadamente 130 GB restantes para el contexto | 8,36 $/hora |
| de vanguardia-mini | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B FP8 (31 GB): la ruta del motor de vanguardia, ensayada de forma económica | aproximadamente 1,7 $/hora |
| de vanguardia-mini-awq | 1 × RTX PRO 6000, **SGLang** | Qwen3-Coder-30B AWQ (17 GB): los núcleos MoE de 4 bits de vanguardia, ensayados de forma económica | aproximadamente 1,7 $/hora |
| trabajo | 1 × RTX PRO 6000 (96 GB); A100 u H100 de 80 GB si no hay ninguna disponible | ninguno: un **pod de trabajo** ejecuta su trabajo, no un servidor de modelos | 2,09 $/hora (A100 de reserva 1,59 $) |
| jam | 1 × A40 (48 GB) primero; A6000, A5000, 3090, L4 o 4090 si no hay ninguna disponible | ninguno: un **pod de trabajo** para las representaciones de canto de ai-jam-sessions (SoulX-Singer) | 0,49 $/hora (A40) |

Un perfil con una `recipe` ejecuta otro motor que no es Ollama: una imagen anclada
(`lmsysorg/sglang:v0.5.20-cu130`), un modelo de Hugging Face que descarga al inicio y
argumentos de servidor adicionales. offrig establece el paralelismo de tensores a partir del recuento de GPU, la longitud del contexto a partir del perfil y mantiene el motor en el bucle del pod; una receta no puede
anular esos valores. Para un repositorio protegido, `hf_token_secret` indica un secreto de RunPod, al que se hace referencia como
`{{ RUNPOD_SECRET_<name> }}` para que el token nunca entre en la especificación del pod. El inicio espera
a que el motor tenga `/health` y la lista de modelos, informa de los pesos en el disco mientras los descarga
y se detiene de inmediato (con el registro del motor) si el motor se cierra.

Cada perfil enumera los tipos de GPU en orden de prioridad; RunPod toma el primero con capacidad.
Cuando no hay ninguno disponible, un perfil puede esperar (`wait_for_gpu_minutes`; el de vanguardia espera hasta 120 minutos):
offrig comprueba cada minuto y crea el pod en el momento en que las GPU se liberan. No se alquila nada
mientras se espera, Ctrl+C o la opción Cancelar de la aplicación lo detienen y, si la API de precios de RunPod no funciona, simplemente vuelve a intentar la creación cada minuto. Los grandes conjuntos de varias GPU aparecen y desaparecen en cuestión de minutos.
Los precios son los precios de la nube segura, que se leen en tiempo real; la página de precios no es el precio disponible.

### Esperando capacidad

Un plan con restricciones de `no_fallback` o `max_price_hr` a menudo no cumple con la capacidad, por lo que el lanzamiento intenta repetidamente en lugar de fallar, sin alquilar nada mientras lo hace. El tiempo que espera es, en orden: el `wait_minutes` del plan (un argumento `offrig_plan`, almacenado con el plan; `0` falla de inmediato), o, en caso contrario, el `wait_for_gpu_minutes` del perfil. El perfil `job` tiene un valor predeterminado de 20 minutos. El tiempo de espera se reduce al tiempo que queda en el plan menos una reserva de cinco minutos, por lo que nunca se excede el plazo del plan, y, dado que no se alquila nada mientras se espera, no se añade nada al peor de los casos previsto. `offrig_launch` informa `capacity_wait_minutes`; mientras se espera, `offrig_job` muestra `progress.capacity_wait` (`checks`, `waited_secs`, `limit_secs`) y un paso que indica de qué comprobación se trata. Cuando el tiempo de espera se agota, el lanzamiento falla con `no capacity` y no se alquila nada.

### Definir el hardware de un plan

Un perfil enumera los tipos de GPU en orden de prioridad y RunPod toma el primero que tenga capacidad disponible, por lo que, sin límites, un plan puede terminar utilizando una tarjeta de reserva con menos memoria, un controlador más antiguo y un precio diferente. Tres límites restringen el plan al hardware que puede utilizar. La planificación sigue siendo gratuita; los límites simplemente restringen lo que el plan puede alquilar.

| Límite | Dónde | Efecto |
|---|---|---|
| `min_cuda` | perfil (`config.toml`) | La versión más antigua de CUDA (controlador) del host, de la lista de RunPod (`13.0`, `12.9`, ... `11.8`). La creación del pod envía cada versión igual o superior como `allowedCudaVersions`. Para un perfil de trabajo, se aplica la versión más reciente de esta y la versión propia `[profiles.job] min_cuda` de la imagen. |
| `min_vram_gb` | perfil | La cantidad mínima total de VRAM (toda la VRAM de los GPU del perfil en conjunto) que acepta un plan. Las ofertas por debajo de esta cantidad se descartan; también se descartan los tipos para los que RunPod no enumera ninguna memoria. |
| `max_price_hr` | argumento `offrig_plan` | El costo máximo que puede tener el pod, en total $/hora para todos sus GPU (la cifra que muestra `offrig_offers`). Las ofertas por encima de esta cantidad se descartan, al igual que los tipos para los que no se indica ningún precio actualmente (no se puede establecer un límite). |
| `no_fallback` | argumento `offrig_plan` | Solo se permite la primera familia de GPU del perfil. Las dos ediciones RTX PRO 6000 Blackwell (Servidor y Estación de trabajo) son una familia; todas las demás tarjetas, incluida la A100 SXM y PCIe, son su propia familia. |

Ambos campos del perfil son opcionales y, por defecto, no están configurados, por lo que un `config.toml` escrito por un offrig anterior se carga sin cambios. El perfil `job` establece `min_cuda = "13.0"`.

`offrig_plan` calcula el peor de los casos a partir de lo que queda:
`max_hours x min(max_price_hr, the dearest listed price among the remaining GPUs)`. Sin
`max_price_hr`, este es el precio más alto que se indica en el perfil, como antes. Se rechaza un plan sin nada disponible, indicando el motivo de cada GPU descartada, y no se escribe nada.

El plan almacena la lista de GPU que queda y el límite de CUDA, y `offrig_launch` solo alquila de ellos, nunca de la lista completa del perfil. `offrig_job` y el resultado del lanzamiento del proceso secundario informan del tipo de GPU y la versión de CUDA del host que se alquila realmente, en un objeto `rented`. La API del pod no informa de la versión de CUDA del host, por lo que, una vez que se establece la conexión SSH, el lanzamiento ejecuta `nvidia-smi` una vez a través del canal y la lee del encabezado (`CUDA Version: 12.8`, o `CUDA UMD Version: 13.4` en los controladores más recientes); `rented.cuda_source` indica `nvidia-smi` o `pod API`. Si la versión de CUDA del host es anterior al límite del plan, la GPU no es una de las que el plan ha enumerado, o el precio es superior al del plan, `offrig_job` devuelve una entrada `warnings` y comienza `next_action` con `WARNING`. Nada se termina automáticamente: detener el alquiler depende del llamante (`offrig_shutdown`). Si ni la API del pod ni `nvidia-smi` proporcionan una versión de CUDA, `rented.notes` lo indica y el límite no se comprueba.

### Disco del contenedor

Un pod tiene dos discos: el disco del contenedor, que es local del host, y el volumen que se monta en `/workspace`. En algunos hosts, `/workspace` es un sistema de archivos de red lento: un pod de trabajo midió 32 MB/s allí frente a 354 MB/s en su disco de contenedor, y no pudo obtener unos 130 GB de modelos a tiempo, mientras que el disco del contenedor solo tenía 60 GB. El tamaño del disco del contenedor es el `container_disk_gb` del perfil (de 30 a 60 GB en los perfiles integrados; el perfil `job` tiene 60) y se incluye en la creación del pod como `containerDiskInGb`. `offrig_plan` toma `container_disk_gb` (de 1 a 2000) para anularlo para un plan; el plan lo almacena, el lanzamiento lo envía y el plan de respuesta y `offrig_status` muestran el tamaño en vigor.

- El disco del contenedor no tiene precio: offrig solo calcula el tiempo de GPU, por lo que el peor de los casos del plan es el mismo con cualquier tamaño. RunPod cobra por el disco; no se ha comprobado si la tarifa que informa para el pod (`offrig_job` la muestra) incluye el disco del contenedor.
- offrig no mueve sus descargas por usted. Los comandos de trabajo comienzan con `HF_HOME` en el volumen `/workspace` (`/workspace/hf`); para utilizar el disco del contenedor, defina el suyo propio (`HF_HOME=/root/hf python ...`) en el comando.
- El disco del contenedor se elimina con el pod, como el volumen sin un volumen de red: copie los resultados con `offrig_get` antes de `offrig_shutdown`.
- El límite de 1 a 2000 es una comprobación de seguridad de offrig para evitar un error tipográfico; no se comprueba el límite real de RunPod.

### Pods de trabajo

Un perfil con un `job` alquila una GPU para un trabajo que se ejecuta en ella, como una ejecución de entrenamiento, en lugar de para servir un modelo. Su pod ejecuta una imagen de PyTorch definida (`runpod/pytorch:2.8.0-py3.11-cuda12.8.1-cudnn-devel-ubuntu22.04`, CUDA 12.8 para Blackwell) con sshd y nada más:

- No sirve para ningún modelo, por lo que no hay ningún túnel y nada está conectado a Zed. sshd no permite ningún tipo de reenvío (`AllowTcpForwarding=no`); la única forma de acceder es mediante ssh al pod.
- Un perfil de trabajo no incluye ningún modelo y tampoco puede tener una receta; la comprobación de la configuración lo rechaza.
- `offrig up` y la aplicación rechazan un perfil de trabajo antes de alquilar nada. Se ejecuta a través del contenedor auxiliar: `offrig_plan profile=job`, `offrig_launch`, luego `offrig_put`, `offrig_exec` y `offrig_get`. El inicio está listo cuando sshd responde.
- Un comando se ejecuta de forma independiente en el pod (`setsid nohup`) en `/workspace/job`, por lo que sobrevive al contenedor auxiliar y a la sesión ssh. Se envía como base64, por lo que nada de su contenido es leído por el shell ssh. Su registro y estado de salida se guardan en `/workspace/offrig/jobs/`. Las descargas de Hugging Face se guardan en `/workspace/hf` en el volumen del pod.
- `offrig_exec action=run` sirve para comprobaciones rápidas (`ls`, `nvidia-smi`), no para trabajo: ejecuta el comando hasta su finalización en `timeout` (por defecto 30 s, como máximo 120 s) y devuelve `stdout`, `stderr`, `exit_code` y `timed_out`. La salida se trunca a los últimos 64 KB de cada flujo (`truncated`) y es una salida de pod no fiable. Un comando que necesita más tiempo es un `start`.
- El registro final de un trabajo tiene las barras de progreso contraídas: las actualizaciones de estilo tqdm, unidas por retornos de carro, muestran solo su último fotograma. `offrig_exec action=status save_log=<local path>` también copia todo el registro del trabajo, tal como está escrito, a un archivo local (se crean las carpetas principales), para que el registro final pueda ser corto.
- La imagen es una compilación de CUDA 12.8, por lo que un perfil de trabajo indica la versión de CUDA más antigua del host en la que se ejecuta (`min_cuda = "12.8"`) y el pod se crea con `allowedCudaVersions` de RunPod a partir de ella. Sin eso, un host con un controlador más antiguo inicia el pod y torch no encuentra ninguna GPU, después de que se haya iniciado el alquiler. El trabajo en sí puede necesitar más que la imagen: el perfil `job` también establece `min_cuda = "13.0"` en el perfil (ver arriba), porque los trabajos que ejecuta instalan un vLLM actual, cuyo PyTorch es una compilación de CUDA 13.
- El presupuesto, el plan, el sistema de vigilancia y el apagado funcionan como en cualquier otro perfil. Copie los resultados antes de `offrig_shutdown`: el disco del pod se guarda.
- `jam` es el perfil de trabajo que ai-jam-sessions utiliza para renderizar sus canciones: SoulX-Singer necesita mucho menos que una tarjeta de entrenamiento, por lo que alquila una barata de 24 a 48 GB. La configuración y la sesión se encuentran en ese repositorio (`docs/vocal-offrig.md`); offrig no sabe nada sobre cantar.

### Preparación de los pesos en un volumen de red

Un perfil de receta descarga sus pesos en cada inicio: para el modelo de vanguardia, esto tomó aproximadamente 20 de 22 minutos para estar listo (252 GB, 8,36 $/hora). La preparación los coloca en un volumen de red de RunPod una sola vez:

```text
offrig stage frontier --dc EUR-IS-1          shows the monthly cost, changes nothing
offrig stage frontier --dc EUR-IS-1 --yes    creates the volume and downloads the weights
offrig stage frontier --remove --yes         deletes the volume (the undo)
```

- El volumen se factura mensualmente, independientemente de si un pod se está ejecutando o no (300 GB para el modelo de vanguardia cuestan aproximadamente 21 $/mes a 0,07 $/GB), por lo que solo un humano realiza la preparación; ninguna herramienta de agente puede hacerlo.
- Un volumen se encuentra en un único centro de datos, por lo que los pods del perfil se inician solo allí, y las ofertas y los planes tienen precios allí. Elija uno con almacenamiento en red y las GPU del perfil; `offrig gpus` y la consola de RunPod muestran dónde se encuentran.
- La descarga se ejecuta en el pod de GPU más barato disponible en ese centro de datos. El pod se termina en caso de éxito, de fallo o de tiempo de espera.
- El volumen se registra en el perfil antes de que comience la descarga, por lo que una preparación fallida nunca se olvida; vuelva a ejecutar para reanudar, o `--remove`.
- Un inicio con preparación previa ejecuta Hugging Face sin conexión, solo cuando la preparación se completa (un marcador en el volumen). Un volumen parcialmente preparado descarga el resto en lugar de fallar.

## Seguridad financiera

- Antes de un inicio, offrig muestra la coincidencia gratuita más barata y su tiempo de ejecución con el pod en funcionamiento. Si el tiempo de ejecución es inferior a una hora, se niega a menos que se anule, porque, al llegar a cero, RunPod detiene todos los pods de la cuenta, incluidos los que offrig no gestiona.
- El apagado automático termina el pod después de 30 minutos con cada GPU por debajo del 5% (configurable o desactivado).
- Cerrar la aplicación con un pod en ejecución pregunta si se debe terminar o mantenerlo en ejecución.
- offrig solo toca los pods que ha nombrado: `offrig-<profile>` para la CLI y la aplicación, `offrig-<tag>-<profile>` para el contenedor auxiliar de un proyecto (ver Contenedores). Un contenedor auxiliar nunca toca los pods de otro contenedor, los del contenedor principal o cualquier otro pod; estos se enumeran, pero nunca se modifican.
- Para las sesiones de agente, el límite se aplica antes de cualquier gasto: el peor de los casos de un plan (precio en vivo × horas máximas) se compromete con el presupuesto establecido por el usuario y se rechaza si se supera, y un inicio solo requiere un ID de plan, por lo que un agente no puede establecer su propio precio.
- Cada inicio de contenedor auxiliar tiene un sistema de vigilancia que termina el pod en la fecha límite del plan, y el ejecutor apaga el pod tan pronto como su cola esté vacía.

## Qué cambia en su máquina

| Qué | Dónde | Deshacer |
|---|---|---|
| Proveedor de Zed `offrig` | `%APPDATA%\Zed\settings.json` | `offrig zed-remove`; el original se conserva como `settings.json.offrig.bak` |
| Modelo predeterminado de Zed (solo si lo solicita) | el mismo archivo | `offrig zed-remove` restaura el valor predeterminado anterior |
| `OFFRIG_API_KEY` (marcador de posición; Zed necesita una clave) | entorno de usuario | `setx OFFRIG_API_KEY ""` o elimínelo en las propiedades del sistema |
| Alias SSH `offrig` | `~/.ssh/config`, entre los marcadores `# >>> offrig:offrig >>>` | elimine el bloque marcado |
| Alias SSH `offrig-<tag>`, uno por proyecto que se inició desde un contenedor auxiliar | `~/.ssh/config`, entre los marcadores `# >>> offrig:offrig-<tag> >>>` | elimine el bloque marcado |
| Canales de proyecto | `%APPDATA%\offrig\lanes.toml` (ruta del proyecto, etiqueta, alias, puerto del túnel) | elimine la entrada del proyecto mientras no haya ningún pod en ejecución en su canal, o elimine todo el archivo |
| Claves de host de pod | `~/.ssh/known_hosts_offrig` | elimine el archivo |
| Configuración | `%APPDATA%\offrig\config.toml` | elimine el archivo |
| Pesos preparados (solo con `offrig stage --yes`) | un volumen de red de RunPod `offrig-<profile>`; se factura mensualmente | `offrig stage <profile> --remove --yes` |

Los comentarios y el diseño en la configuración de Zed se conservan: las ediciones se realizan a través de un árbol de sintaxis JSONC.

## Modelo de amenazas

- **Clave de la API de RunPod.** Se lee de `RUNPOD_API_KEY`; nunca se escribe en el disco o en los registros. El proveedor de Zed, a propósito, no se llama `runpod`: con ese nombre, Zed leería `RUNPOD_API_KEY` y la enviaría al servidor del modelo.
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
- **El proceso secundario:** de extremo a extremo a través de stdio con un RunPod simulado, el proceso de vigilante real y el proceso de ejecutor real con un modelo de pod simulado; cada error de herramienta lleva un código.

`scripts/verify.sh` (o `scripts/verify.ps1`) ejecuta la comprobación de formato, clippy, las pruebas y una ejecución de prueba de cada binario en un solo comando. La CI también ejecuta `cargo deny`, un análisis OSV de `Cargo.lock`, cobertura a Codecov y `atlas check`.

### Registro de pruebas en vivo (2026-10-02, nivel medio, A100 de 80 GB, aproximadamente 0,45 $)

- El pod se inicia en aproximadamente 80 s; sshd, el túnel y el Ollama 0.35.0 del pod responden.
- Se descargan 97 GB de modelos en el pod a una velocidad de aproximadamente 150-250 MB/s.
- `qwen3-coder:30b-a3b-q8_0` y `gpt-oss:120b` respondieron a un chat transmitido con una llamada de herramienta correcta a través del túnel. Utilizaron 36 GB y 64 GB de la VRAM del pod; el Ollama local no cargó nada y no estaba en la GPU local.
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

Encontrado y corregido: el pod atendía una solicitud a la vez (`OLLAMA_NUM_PARALLEL=1`); cuatro ranuras tardaron 8 solicitudes paralelas de 40 a 102 tok/s en la misma GPU, por lo que ahora cada perfil tiene `parallel = 4`. `complete` se rechazó sin una razón (ahora tiene como valor predeterminado "se cumplió la comprobación de aceptación"; los fallos aún necesitan una). El estado sugería registrar brevemente mientras una sesión estaba activa. Se piensa que el texto que se filtra en una respuesta se elimina y que una respuesta vacía por pensar indica que se debe aumentar `max_tokens`.

### Ensayo del ejecutor (2026-10-03, nivel pequeño, RTX 2000 Ada, 0,04 $ reservados)

Cinco transferencias, una de las cuales depende de otra, funcionaron mediante `offrig_run` sin que nadie las controlara:

- Cuatro transferencias en curso a la vez en cuatro ranuras (12,9 GB de 16 GB de VRAM); la dependiente se inició en el momento en que la dependiente completó y se basó en su resultado.
- Las tres transferencias cuyas comprobaciones cubrieron la aceptación se completaron por sí solas; las historias rivales (comprobaciones parciales) y el folclore (sin comprobaciones) se enviaron para su revisión.
- La revisión devolvió el folclore ("el río lleva el nombre del proyecto"); el ejecutor en vivo lo adoptó y lo revisó en función de los comentarios ("río Veyl").
- La cola se vació en 6,5 minutos (6 turnos, 20 861 tokens); el ejecutor cerró el pod por sí mismo.

Aprendido: las comprobaciones deterministas verifican la estructura, no la calidad del diseño. El modelo de 4B superó "tres verbos" con verbos débiles, por lo que `accept_on_checks` es para el trabajo estructural y el trabajo de diseño se envía a revisión. qwen3:4b gastó aproximadamente 4000 tokens pensando por turno, incluso en tres líneas de folclore. Una cola mantiene cada ranura ocupada solo cuando contiene suficientes transferencias independientes; una cadena de dependencias se ejecuta una a la vez.

### Ejecuciones de Frontier y SGLang (2026-10-03, 4,27 $ reservados)

| Ejecutar | Pod | Listo después de | Cola | Reservado |
|---|---|---|---|---|
| frontier-mini (Qwen3-Coder-30B FP8) | 1 × RTX PRO 6000 | 5,5 min | 4 transferencias en 15 s | $0.30 |
| frontier-mini-awq (Qwen3-Coder-30B AWQ) | 1 × RTX PRO 6000 | 4 min | 4 transferencias | $0.25 |
| **frontier (Qwen3-Coder-480B AWQ)** | **4 × RTX PRO 6000** | **22 min** (252 GB a 278 MB/s, luego carga) | **31 transferencias en 20 s** | **$3.59** |
| 30B, exploración de conjunto | 1 × RTX PRO 6000 | 10 min (colocación lenta de pods) | solo exploración | $0.43 |

- SGLang v0.5.20 (cu130) se ejecuta en Blackwell: atención flashinfer, `awq_marlin` para los
pesos MoE de 4 bits, paralelismo de tensores a través de PCIe en cuatro tarjetas; `/dev/shm` era de 352 GB.
- El trabajo de la versión más avanzada fue claramente mejor que el de los modelos pequeños: diálogos dentro del entorno y un
módulo Rust que se compiló y superó sus tres pruebas (verificado localmente). La versión de la misma tarea con 30B FP8
no se compiló.
- Un modelo cargado sirve a todo un conjunto; no se necesitan copias. Exploración de concurrencia con
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
rendimiento total sigue aumentando; las ganancias de 480B se estabilizan después de 256. La caché KV de la versión más avanzada
contiene 398.526 tokens, por lo que, con contextos de transferencia reales de 2 a 8 mil tokens, la versión más avanzada
ahora ejecuta 64 en paralelo y las versiones de SGLang de una sola tarjeta, 32.

Se encontraron y corrigieron en el camino: las revisiones rellenaron la salida para pasar una verificación de encabezado (ahora una
verificación integrada `no_repeats`, y las revisiones reestructuran en el lugar); la salida requerida de un rol se filtró en los resultados (la transferencia ahora establece el formato); la retroalimentación del encabezado
indica el formato Markdown; una revisión sin cambios se detiene en lugar de repetirse.

Aún no se ha verificado en vivo: un chat
enviado desde el panel de agentes de Zed (se prueba directamente la forma de la solicitud que utiliza Zed).

## Cumplimiento de estándares

Se evaluó según los estándares de flujo de trabajo del estudio (0 faltantes, 1 parcial, 2 presentes,
3 ejemplares).

- **PIN_PER_STEP: 2.** Las imágenes de los pods se fijan a las etiquetas de versión (`ollama/ollama:0.35.0`,
`lmsysorg/sglang:v0.5.20-cu130`; una receta rechaza `latest`), el compilador a 1.98.1,
las dependencias por `Cargo.lock` y el motor Atlas a la versión 1.24.0 de la flota. Cada turno de transferencia
registra su modelo, hash de rol y hash de solicitud. Los modelos se fijan por etiqueta o ID de repositorio,
no por resumen.
- **ANDON_AUTHORITY: 3.** Cada paso detiene la ejecución en caso de un defecto: un plan cuyos pesos
exceden el disco se rechaza antes de cualquier gasto; una descarga fallida detiene el inicio; una edición de Zed
que no se puede leer no se escribe; se informa un archivo de configuración dañado, nunca
se vuelve a escribir; CI bloquea en fmt, clippy, pruebas, licencias y avisos.
- **NAMED_COMPENSATORS: 2.** Cada acción irreversible tiene una anulación, que se enumera a continuación.
- **DECOMPOSE_BY_SECRETS: 2.** Un módulo por cada cosa que cambia por sus propias razones:
la API de RunPod (`runpod`), el contenido del pod (`spec`), el transporte (`tunnel`,
`remote`), cada archivo local que edita offrig (`sshconfig`, `zed`) y las reglas (`guard`,
`cost`). Los front-ends no contienen lógica más allá de la presentación.
- **UNCERTAINTY_GATED_HUMANS: 2.** offrig pregunta solo cuando el resultado es costoso o
con pérdida: inicio con menos de una hora de tiempo disponible, terminación de un pod (indicando lo que se pierde) y
salida con un pod que aún está facturando. Dos decisiones pertenecen únicamente a un humano, y ninguna
herramienta de agente puede tomarlas: el límite de presupuesto y la preparación de un volumen, que se factura mensualmente.
La salida de la transferencia que el código no puede verificar espera en la revisión en lugar de completarse.
- **EXTERNAL_VERIFIER: n/a.** No hay afirmaciones especializadas.

**Compensadores**

| Acción | Deshacer | Estado después de la anulación | Propietario |
|---|---|---|---|
| Crear un pod (comienza la facturación) | `offrig down <profile> --yes`, la aplicación se cierra o se detiene automáticamente | pod terminado, facturación detenida | el operador que ejecuta offrig |
| Terminar un pod | ninguno para su disco; volver a lanzar el perfil y los modelos se vuelven a descargar (un volumen de red los mantiene) | nuevo pod, mismo perfil | el operador |
| Escribir el proveedor de Zed o el modelo predeterminado | `offrig zed-remove`, o restaurar `settings.json.offrig.bak` | Zed como antes, offrig | el operador |
| Establecer `OFFRIG_API_KEY` | `setx OFFRIG_API_KEY ""` o eliminarlo en las propiedades del sistema | variable eliminada | el operador |
| Escribir el alias SSH | eliminar el bloque marcado en `~/.ssh/config` | configuración como antes | el operador |
| Asignar una pista de proyecto (la primera `offrig_plan` del proyecto) | eliminar la entrada del proyecto de `lanes.toml` una vez que no haya pods en su pista; un nuevo plan se asigna nuevamente | pista libre para reutilizar; el bloque de alias es independiente (fila superior) | el operador |
| Escribir el bloque de alias SSH de una pista (lanzamiento de un side-car) | `offrig_shutdown` lo elimina cuando nombra el pod del plan (también después de un lanzamiento fallido); de lo contrario, eliminar el bloque `# >>> offrig:offrig-<tag> >>>` en `~/.ssh/config` | configuración como antes; los bloques de otras pistas no se ven afectados | el operador |
| Descargar un modelo en el pod | `ollama rm <model>` en el pod, o terminar el pod | modelo eliminado | el operador |
| Eliminar un túnel offrig huérfano | no se necesita; solo se elimina un `ssh` con el alias y el reenvío exactos de esta pista, nunca el de otra pista | puerto libre | offrig |
| Lanzamiento de side-car (`offrig_launch`) | `offrig_shutdown`; automático si la configuración falla; el watchdog al final del plazo; el ejecutor cuando su cola se vacía | pod terminado, plan cerrado con gasto medido | el agente que lo llama, con el watchdog como respaldo |
| Preparar un volumen (`offrig stage --yes`, se factura mensualmente) | `offrig stage <profile> --remove --yes` | volumen eliminado, perfil vuelve a la descarga | el humano que lo preparó |
| Iniciar un trabajo en un pod de trabajo (`offrig_exec action=start`) | `offrig_exec action=stop`, o `offrig_shutdown` | trabajo terminado con todo lo que inició; su registro permanece hasta que el pod desaparece | el agente que lo llama |
| Ejecutar un comando corto en un pod de trabajo (`offrig_exec action=run`) | solo lo que el propio comando hace; finalizado por `timeout` en un máximo de 120 s, o por `offrig_shutdown` | el pod tal como lo dejó el comando | el agente que lo llama |
| Copie archivos hacia o desde un pod de trabajo (`offrig_put`, `offrig_get`) | elimine la copia (en el pod, `offrig_exec`; aquí, el archivo) | como antes de la copia | el agente que lo llama |

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
