# Flujo de trabajo con git: ramas, commits, push y versiones

Flujo acordado para decanter y psport (decisión #376 del tablero). Parte de cómo se trabaja hoy,
dice qué cambia y, al final, lo que se decidió en cada punto.

## 1. Cómo se trabaja hoy

| | decanter | psport |
|---|---|---|
| Remoto | GitHub (`yerasuu/decanter`) desde el 16 de octubre; gitquarry queda como remoto `gitquarry` | GitHub (`yerasuu/psport`) |
| Ramas publicadas | `develop` y `main` | solo `master` |
| Integración | `develop` → merge request → `main` (con merge commit) | todo va directo a `master` |
| Quién recibe cada cambio | GitHub Actions al llegar a `main` | el canal Testing de decanter construye **cada commit de `master`** |
| Commits | Conventional Commits (`feat(ps): …`), una línea | inglés llano sin tipo, una línea, a menudo de más de 120 caracteres |
| Versiones | `v0.2.0` | ninguna propia; el canal Release sigue el tag que publica la CI (`nix-cache`) |
| Firma | commits y tags firmados (clave `20E5297574D49B46`) | commits firmados |
| Ramas locales | `develop`, `main`, dos `backup/*` | 135 ramas, 107 ya fusionadas, unos 20 worktrees |
| Push | lo haces tú; los agentes nunca | igual |

Lo que funciona y se mantiene:
- push solo tuyo;
- un commit claro por funcionalidad;
- rebase en vez de merge en las ramas de trabajo;
- no reescribir nada publicado;
- firma.

Hay tres problemas:
1. **psport publica sin pasar por integración.** Lo que entra en `master` llega al canal Testing
   en cuanto lo subes, aunque aún no haya pasado la regress nocturna.
2. **Los mensajes de psport no dicen el tipo de cambio.** Así no se puede derivar la versión ni
   las notas de los commits, como piden las reglas de versionado.
3. **Las ramas y los worktrees se acumulan,** con 135 ramas y unos 60 worktrees borrados ayer a
   mano.

## 2. Ramas

El mismo modelo en los dos repositorios:

| Rama | Para qué | Quién escribe | Se publica |
|---|---|---|---|
| `main` (psport: `master`, ver D1) | lo que reciben los usuarios; cada versión es un tag sobre ella | solo avance rápido desde `develop` | sí |
| `develop` | integración: lo que ha pasado build y comprobaciones de día | fusión de ramas de trabajo | sí |
| `<tipo>/<id>-<tema>` | una tarea del tablero (`feat/361-gpu-translator`, `fix/348-save-sound`) | el agente o tú | solo si dura más de un día o necesita CI (D3) |
| `release/vX.Y.Z` | opcional, si hace falta estabilizar una versión mientras `develop` sigue | | sí, mientras dura |

Reglas:
- **De la rama de trabajo a `develop`:**
  1. Rebase sobre `develop`.
  2. Aplastar los pasos intermedios en uno o pocos commits claros.
  3. Build y comprobaciones sin juegos.
  4. Avance rápido de `develop` (historia lineal, sin merge commits).
- **De `develop` a `main`:**
  - Solo con la regress nocturna limpia y sin bajar más de un 3 % de FPS en Bloodborne.
  - En decanter, igual que en psport: avance rápido local y push tuyo a GitHub.
  - En psport, con avance rápido o PR en GitHub (D2).
- **Tras fusionar,** la rama local se borra y su worktree se quita el mismo día.
  - Los hitos que interesa conservar se guardan como tag anotado (`archive/<rama>`), no como rama.
- **Worktrees:**
  - En psport, el clon principal está en `~/Projects/dev/psport/dev`, sobre `develop`.
    Cada rama activa tiene su worktree al lado, en `~/Projects/dev/psport/<rama>`.
  - Un barrido semanal borra los que tengan la rama fusionada.

## 3. Commits: menos y más útiles

### Qué mide hoy

Entre el 13 y el 15 de octubre, psport recibió 23, 33 y 117 commits en `master`. Los asuntos
miden 96 caracteres de mediana y hasta 193. Se leen como descripciones completas, no como el
asunto de un mensaje.

La misma tarea suele quedar repartida. Black Ops II, por ejemplo, ocupa cinco commits:
- tres de schedules;
- uno de la opción de disparo;
- uno de la fila del README.

Otras veces una reorganización deja un commit aparte solo para corregir rutas en comentarios.

### La unidad: un cambio que una persona contaría en una frase

- **Normalmente, un commit por tarea del tablero** al llegar a `develop`. Debe poder revisarse y
  revertirse solo.
- **Dentro de ese commit va todo lo que el cambio arrastra:** documentación, `FORK.md`, README,
  comentarios que nombran rutas, schedules o herramientas de prueba que lo acompañan.
- **Se separa en dos solo si cada parte tiene sentido por sí misma.** El caso típico es un
  movimiento mecánico grande de ficheros, aparte del cambio de comportamiento que vino después,
  para que el diff se pueda leer. También si la tarea sale con dos tipos de verdad, por ejemplo un
  `fix` que además añade una función.
- **En la rama de trabajo se puede guardar tanto como haga falta** (puntos de control, pruebas).
  Al integrar se aplasta. Esa es la tarea del integrador, que de momento soy yo.
- **Volumen orientativo:** unos 3 a 8 commits al día por repositorio en `develop`, no decenas.

### El mensaje

- **Asunto corto y natural,** de unos 50 caracteres y 72 como máximo:
  - imperativo y en inglés;
  - con `tipo(área):` delante;
  - dice qué cambia para quien lo usa o lo mantiene, no cada detalle.
- **Cuerpo opcional, de una a cuatro líneas,** solo cuando el porqué no es evidente: la causa, la
  evidencia o el efecto medido. Es la parte que más ayuda dentro de un año. Lo que ya está en el
  tablero, `FORK.md` o la documentación no se repite.
- **El pie `BREAKING CHANGE:`** cuando rompe algo de cara a decanter, a los usuarios o a la
  configuración.
- **Sin atribución de IA, firmado,** y con la comprobación del hook `commit-msg`: formato, tipo,
  largo del asunto y que no haya atribución.

Ejemplo, hoy:

```
Count the process time counter in TSC ticks at the TSC frequency, as the console does
```

Propuesto:

```
fix(kernel): count process time in TSC ticks, as the console does

Black Ops converts it with sceKernelGetTscFrequency; counting nanoseconds made
its clock run 5x slow (13 FPS). Now 60 FPS.
```

### Cómo habría quedado el día 15 en psport

Los 117 commits caben en unos 12, cada uno con su porqué:

| Propuesto | Sustituye a |
|---|---|
| `fix(kernel): count process time in TSC ticks, as the console does` | el arreglo del reloj de BO1 |
| `test(regress): play Black Ops through its first mission and save` | schedules de BO1, `follow` y sus cadenas |
| `test(regress): play Black Ops II to the river barge and resume it` | cinco commits de BO2 y la fila del README |
| `fix(gpu): translate the GCN instructions PS2 Classics hit instead of asserting` | el recompilador de Blood Omen 2 |
| `fix(system): fix six lock races and lost wakeups in AvPlayer, VideoOut and NP` | los seis arreglos de la auditoría |
| `refactor(psport): drop Bloodborne from the core's comments and default game dir` | dos commits de la generalización |
| `refactor(psport): group src/ and gpu/ by area` | movimientos y rutas en docs y comentarios |
| `refactor(psport): split probe.c, loader_patches.c and psport_gpu.cpp by job` | tres divisiones |
| `refactor(kernel): resolve imports through an ordered table` | la tabla de resolución |
| `refactor(gpu): read the upscaler's scene sizes from the game profile` | tamaños de escena |
| `docs(psport): describe psport as an independent project` | READMEs, FORK.md y la regla |
| … más los de la conversión de nombres y los trofeos de ese día, agrupados igual | |

## 4. Push

- **Lo haces tú.** Los agentes nunca hacen push y dicen qué queda local sin publicar.
- **Antes de cualquier reescritura:** `git fetch` y comprobar `origin/<rama>`. Lo publicado no se
  reescribe nunca.
- **Ramas de trabajo publicadas:** solo `--force-with-lease`, y solo en ramas tuyas o del agente
  que nadie más usa.
- **Orden recomendado al publicar:**
  1. `develop` de psport.
  2. `develop` de decanter.
  3. Los avances a `main` y sus tags, cuando la noche los valide.
- **Remotos de revisión:** `bbport` y `shadps4` tienen el push desactivado.

## 5. Versiones y notas

- **SemVer derivado de los commits del rango**, según las reglas de CLAUDE.md:
  - en `0.y.z`, `!` sube `y`;
  - `feat`, `fix` y `perf` suben `z`;
  - lo demás no saca versión.
- **psport tiene versión propia** (D5). Sus tags `vX.Y.Z` son las versiones del canal Release.
  La CI publica la build de cada tag; hoy lo hace con el tag `nix-cache`.
- **Notas de cada versión:**
  - un fichero `tag-changelog/vX.Y.Z.md` con viñetas técnicas leídas del árbol en ese tag;
  - `CHANGELOG.md` por áreas, como piden las reglas.
  - El build falla si falta el fichero de la versión en curso. En decanter, un `include_str!` por
    `CARGO_PKG_VERSION`; en psport, una comprobación en `build.sh`.
- **Secuencia de una versión:**
  1. El fichero de notas.
  2. El commit de subida de versión.
  3. El avance de `develop` a `main`.
  4. El tag anotado y firmado sobre `main`.
  5. En decanter, cuando la CI ha subido el AppImage a la versión, `scripts/sign-release X.Y.Z`
     lo descarga, lo firma con minisign y sube `Decanter-x86_64.AppImage.minisig` (decisión #400).
     Sin esa firma, decanter no ofrece instalar la versión por sí mismo.
- **Clave de firma de las versiones de decanter** (se crea una vez y es tuya; ni la CI ni los
  agentes la tocan):
  - `minisign -G -s ~/.minisign/decanter.key -p keys/release.pub` crea la secreta, con
    contraseña, y la pública en el repositorio.
  - `keys/release.pub` se commitea; el build la incrusta y falla si no es una clave minisign. Vacía,
    el build sigue y el AppImage solo avisa de las versiones nuevas.
  - La firma lleva el comentario de confianza `decanter vX.Y.Z`, que decanter comprueba antes de
    instalar. `DECANTER_MINISIGN_KEY` apunta a otra ruta de la clave secreta.
  - Una versión firmada solo la instala un decanter que traiga esa misma clave pública: cambiar de
    clave obliga a actualizar a mano una vez.
- **Repin solo de dependencias** (`Cargo.lock`, `flake.lock`): es un `chore` y no saca versión.

## 6. Tablero y noches

- **Cada rama corresponde a una tarea** con el mismo tipo en su etiqueta. `kb list --tag feat` y
  `--tag fix` son la base de las notas.
- **De día:** código, build y comprobaciones sin juegos, con fusión a `develop` si todo pasa.
- **De noche:**
  - regress, FPS y comparación de capturas de `develop` (`tools/regress/night.sh`);
  - si sale limpio, el informe de la mañana propone avanzar `main` y la versión que toca;
  - el push lo haces tú.

## 7. Decisiones tomadas (16 de octubre de 2026)

| | Decisión |
|---|---|
| D1 | La rama principal de psport pasa de `master` a `main`, igual que en decanter. |
| D2 | `main` avanza por avance rápido local desde `develop`, y el push lo haces tú. |
| D3 | Las ramas de trabajo no se publican, salvo que duren más de un día o necesiten CI. |
| D4 | Asunto corto y cuerpo opcional de una a cuatro líneas con el porqué. Las reglas de CLAUDE.md de decanter, de psport y la global ya lo recogen. |
| D5 | psport empieza su numeración en `v0.1.0`. |
| D6 | El canal Testing de decanter sigue `develop`. `main` solo recibe lo validado de noche, y Release, los tags. |
| D7 | Se borran las ramas ya fusionadas de psport. |
| D8 | Los `backup/*` de decanter pasan a tags. |
| D9 | Aplasto yo al integrar en `develop`. Los agentes proponen el mensaje en su informe. |

## 8. Lo ya publicado

Aprovechando el cambio de `master` a `main`, la historia propia de psport después de `701f0a7`
(460 commits) se reagrupa con este mismo criterio en commits útiles sobre `main`:
- **Se conservan** las fechas de los commits importantes y la firma.
- **Árbol final idéntico** al de la `master` publicada.
- **La `master` antigua queda archivada** en el tag `archive/master-2026-10-16`.
- **No se toca** lo anterior a `701f0a7`, que es la historia de bbport y sus fusiones.
- **Para quien ya tenga la `master` antigua,** decanter detecta que todos sus commits siguen
  publicados en ese tag y pasa la copia a `develop` sin pedir nada.
