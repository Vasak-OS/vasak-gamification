# Vasak gamification service

El servicio de gamificación de VasakOS: XP, rachas y logros del sistema. Un módulo que hace algo puntuable avisa de qué tipo fue el evento por D-Bus; el servicio decide cuánto vale cada tipo, acumula el progreso y lo guarda. Configuración lo consulta para mostrarlo.

---

## Tecnología

[![Rust](https://img.shields.io/badge/Rust-1.82%2B-orange?style=flat&logo=rust)](https://www.rust-lang.org/) [![systemd](https://img.shields.io/badge/systemd-256%2B-24283b?style=flat&logo=systemd)](https://systemd.io/) [![D-Bus](https://img.shields.io/badge/D-Bus-1.15%2B-42b883?style=flat&logo=dbus)](https://dbus.freedesktop.org/) [![zbus](https://img.shields.io/badge/zbus-5.0-orange?style=flat&logo=rust)](https://github.com/dbus2/zbus)

## Estado

[![CI](https://github.com/Vasak-OS/vasak-gamification/actions/workflows/ci.yml/badge.svg)](https://github.com/Vasak-OS/vasak-gamification/actions/workflows/ci.yml)
[![Versión](https://img.shields.io/github/v/release/Vasak-OS/vasak-gamification?style=flat&label=Versi%C3%B3n)](https://github.com/Vasak-OS/vasak-gamification/releases)
[![Licencia](https://img.shields.io/github/license/Vasak-OS/vasak-gamification?style=flat)](LICENSE)
[![Cobertura](https://sonarcloud.io/api/project_badges/measure?project=Vasak-OS_vasak-gamification&metric=coverage&style=flat)](https://sonarcloud.io/summary/new_code?id=Vasak-OS_vasak-gamification)
[![Quality Gate](https://sonarcloud.io/api/project_badges/measure?project=Vasak-OS_vasak-gamification&metric=alert_status&style=flat)](https://sonarcloud.io/summary/new_code?id=Vasak-OS_vasak-gamification)
[![Tamaño repo](https://img.shields.io/github/repo-size/Vasak-OS/vasak-gamification?style=flat)](https://github.com/Vasak-OS/vasak-gamification)

---

## Descripción

El servicio de gamificación de VasakOS. Lleva la cuenta de XP, rachas y logros de quien usa el sistema.

**Para el usuario:** apagado por omisión. Prendida la capa en Configuración, pequeñas acciones del día a día —tomar una pausa, hidratarse, lo que cada módulo decida emitir— suman puntos, mantienen una racha de días y desbloquean logros.

**Integración VasakOS:** corre en el bus de sesión. Cualquier módulo que haga algo puntuable sólo avisa **de qué tipo** fue el evento con `Emit(kind)`; el servicio decide cuántos puntos vale cada tipo, así la economía de puntos se afina en un solo lugar. Configuración consulta `Progress()` para mostrar el avance y `Reset()` para vaciarlo.

---

## Arquitectura (texto)

### Crates del workspace

| Crate | Qué es |
|---|---|
| `vasak-gamification-protocol` | El contrato D-Bus (tipos + proxy), compartido con Configuración. Se publica a crates.io |
| `vasak-gamification-daemon` | El servicio: binario `vasak-gamification-service` |

### Interfaz D-Bus (`ar.net.vasak.os.Gamification`)

| Método | Firma | Para qué |
|---|---|---|
| `Emit` | `emit(kind: &str)` | Un módulo avisa que pasó algo puntuable de tipo `kind`. No manda puntos: los decide el servicio. Apagada la capa, no hace nada |
| `Progress` | `progress() -> GamificationProgress` | El progreso: XP, racha y logros. Apagada la capa, viene en cero |
| `Reset` | `reset()` | Vacía el progreso |

### El estado

Texto plano en `~/.local/share/vasak-gamification/progress.json`, con escritura atómica (temporal + `rename`). No es secreto: no va al llavero. Al arrancar, si todavía no hay estado propio, se importa una sola vez el progreso que `vasak-health-service` haya dejado en `~/.local/share/vasak-health-service/gamification.json`, para no perder lo de quien prendió la capa cuando vivía ahí.

### Diagrama

```mermaid
flowchart LR
    Emisor[Módulo emisor] -->|Emit(kind)| Service[vasak-gamification-service]
    Settings[Configuración] -->|Progress / Reset| Service
    Service -->|decide puntos, acumula| Engine[Motor: award / racha / logros]
    Engine -->|JSON atómico| Disk[(progress.json)]
    Conf[vasak.conf: gamification.enabled] -->|al arrancar| Service
```

---

## Instalación

### Arch Linux (repo oficial)

```bash
pacman -S vasak-gamification
```

### Desde fuente

```bash
git clone https://github.com/Vasak-OS/vasak-gamification.git
cd vasak-gamification
cargo build --release --locked
```

El binario queda en `target/release/vasak-gamification-service`. El paquete lo
arma `PKGBUILDS/vasak-gamification/`, que instala el binario en `/usr/bin`, el
unit de usuario y el `.service` de activación D-Bus de `packaging/`.

---

## Uso

Corre como servicio de usuario (`systemd --user`), activado por D-Bus:

```bash
systemctl --user enable --now vasak-gamification-service
# o a mano:
vasak-gamification-service
```

Apagado por omisión: no cuenta nada hasta que la capa se prenda.

---

## Configuración

El interruptor vive en `vasak.conf` (la misma del resto del sistema), con la
clave `gamification.enabled`. Lo escribe Configuración; el servicio lo lee **una
vez al arrancar** (hoy no cambia en caliente).

```json
{
  "gamification": { "enabled": true }
}
```

| Variable de entorno | Descripción | Por defecto |
|---|---|---|
| `VASAK_CONFIG_PATH` | Pisa la ruta de `vasak.conf` | `~/.config/vasak/vasak.conf` |
| `RUST_LOG` | Nivel de los logs (`vasak_gamification_daemon=debug`…) | `info` |

---

## Desarrollo

### Requisitos

- **Rust:** toolchain estable (`rustup`), con `rustfmt` y `clippy`.
- **Sistema:** `build-essential` / `base-devel` (el compilador de C de los `-sys`).

### Verde local antes del PR

```bash
cargo fmt --check
cargo clippy --all-targets
cargo build --locked
cargo test --locked
```

### Cómo abrir PR

1. Rama desde `origin/main`, en un worktree aparte:
   ```bash
   git worktree add -b feat/tema ~/wt/vasak-gamification-tema origin/main
   ```
2. Prefijos de rama: `fix/`, `feat/`, `bugfix/`, `docs/`, `ci/`, `chore/`, `security/`.
3. Escribe el fix → **`qa` escribe las pruebas** (fallan sin el fix) → suite verde → abre PR.
4. Al abrir PR, **cuatro cosas obligatorias**: asignado a `JoaquinDecima`, etiqueta `enhancement` o `bug`, añadido al proyecto «VasakOS Roadmap», status **In Progress**.

---

## Pruebas y calidad

- **Cobertura objetivo:** ≥ 80 % del código nuevo (SonarCloud Quality Gate).
- Las pruebas de Rust corren en CI sobre los paquetes que nombra `pruebas_rust`
  en `.github/workflows/ci.yml`: `vasak-gamification-protocol` y
  `vasak-gamification-daemon`.
- **Regla:** toda corrección lleva prueba escrita por `qa` que falla sin el fix.

---

## Licencia

`GPL-3.0-or-later` — Copyright © 2026 **Vasak-OS Contributors**

[Ver licencia completa](LICENSE)
