# Detección logros GOG/Epic bajo Proton — concepto final

Diagrama arquitectura completa. Tres zonas: reusado de bottles-next (sin tocar), construido por nosotros (achv-*), y servicios externos (GOG/Epic).

```mermaid
flowchart TB
    subgraph EXT["Externo"]
        GOGAPI["gameplay.gog.com<br/>gog::achievements(product_id, uid)"]
        EPICAPI["store.epicgames.com/graphql<br/>(sin soporte crate, TODO)"]
    end

    subgraph BN["bottles-next — reusado, cero código propio"]
        CRED["CredentialStore<br/>(profile_id, Storefront) → token blob"]
        BOTTLE["Bottle::run(program_id)<br/>Bottle::processes()<br/>Bottle::set_dll_override()"]
        WB["WineBridge<br/>CreateProcessW dentro prefix"]
        LOGIN["next-ui (Iced)<br/>login GOG/Epic ya hecho por usuario"]
    end

    subgraph ACHV["achv — construido por nosotros"]
        CLI["achv-sidecar launch<br/>(vive lo que dura la sesión)"]
        LINKDB[("links.db<br/>GameLink: program_id → storefront+product_id")]
        POLL["achv_poll<br/>lee token, llama API logros"]
        HOOK["achv-hook.dll<br/>proxy Galaxy64/EOSSDK dentro Wine"]
        TRAY["achv-tray (Dioxus)<br/>proceso largo: dashboard + toast"]
    end

    GAME["proceso del juego<br/>(Wine/Proton)"]

    LOGIN -->|guarda token| CRED
    CLI -->|lee link| LINKDB
    CLI -->|set_dll_override antes de correr| BOTTLE
    CLI -->|run program_id| BOTTLE --> WB --> GAME
    GAME -->|SetAchievement / EOS_Unlock| HOOK
    HOOK -->|TCP loopback :47812<br/>evento unlock instantáneo| TRAY
    CLI -->|poll cada 2s hasta exit| BOTTLE
    CLI -->|al iniciar y al salir| POLL
    POLL -->|lee token| CRED
    POLL -->|achievements product_id,uid| GOGAPI
    POLL -.->|pendiente| EPICAPI
    POLL -->|diff vs cache, emite unlock confirmado| TRAY
    TRAY -->|toast / dashboard| USER(("usuario"))

    style HOOK stroke-dasharray: 4 4
    style EPICAPI stroke-dasharray: 4 4
```

## Qué significa cada línea

- **Sólido** = camino implementado/confirmado contra código real de bottles-next.
- **Punteado** = capa1 (hook DLL) y Epic achievements — experimental / sin resolver aún, no bloquea el resto.

## Reparto de trabajo

| Pieza | Quién la construye |
|---|---|
| Auth GOG/Epic | bottles-next (`CredentialStore` + next-plugin-gog/egs) |
| Runner/prefix/DLL override | bottles-next (`Bottle`, `WineBridge`) |
| Logros GOG | achv, wrapper trivial sobre `gog::achievements()` |
| Logros Epic | achv, sin soporte upstream, pendiente |
| Launch/exit detection | achv, propio (bottles-core no emite estos eventos) |
| GameLink (program↔juego) | achv, propio (`Program` no tiene ese campo) |
| Hook DLL tiempo real | achv, experimental, zona gris ToS |
| Notificación/dashboard | achv-tray, Dioxus |
