//! El contrato entre el servicio de gamificación y quien lo use —Configuración,
//! el escritorio, cualquier módulo que quiera puntuar algo—.
//!
//! Todo lo que viaja por D-Bus se define acá una sola vez, así los dos lados no
//! se separan: un campo renombrado en una punta deja de compilar en la otra en
//! lugar de volverse, en silencio, un informe que nunca cuadra.
//!
//! # Quién emite y quién decide
//!
//! Un módulo del sistema que hace algo puntuable —tomar una pausa, hidratarse,
//! lo que sea— sólo avisa **de qué tipo** fue el evento con [`Gamification::emit`].
//! **No manda puntos**: cuánto vale cada tipo lo decide el servicio, no el
//! emisor, así la economía de puntos se afina en un solo lugar sin recompilar a
//! quien emite. El servicio lleva la cuenta, la guarda en texto plano y la
//! devuelve cuando Configuración consulta el progreso.

use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

// ── Direcciones del bus ───────────────────────────────────────────────────────

/// El servicio corre en el bus de **sesión**, como el usuario con la sesión
/// abierta: la gamificación es de quien está sentado en la máquina, y nada de
/// esto sale de ahí.
pub const SERVICE_NAME: &str = "ar.net.vasak.os.Gamification";
pub const SERVICE_PATH: &str = "/ar/net/vasak/os/Gamification";
pub const SERVICE_INTERFACE: &str = "ar.net.vasak.os.Gamification";

// ── El progreso ───────────────────────────────────────────────────────────────

/// El progreso de gamificación, para que Configuración lo muestre. Apagada la
/// capa, `enabled` es `false` y el resto viene en cero: no se cuenta nada hasta
/// que la persona la prenda.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct GamificationProgress {
    /// Si la capa está prendida.
    pub enabled: bool,
    /// Experiencia total acumulada.
    pub xp: u64,
    /// Días seguidos con al menos un evento.
    pub streak_days: u32,
    /// Ids de los logros ya desbloqueados.
    pub achievements: Vec<String>,
}

// ── El proxy, para quien esté en zbus 5 ───────────────────────────────────────

/// El lado cliente del servicio.
///
/// Lo usa quien esté en la misma versión de zbus que el servicio. Quien esté
/// clavado en zbus 4 por otros motivos replica estas firmas a mano, como ya hace
/// el escritorio con otros servicios; cuando suba a zbus 5 podrá usar este proxy
/// directamente.
#[zbus::proxy(
    interface = "ar.net.vasak.os.Gamification",
    default_service = "ar.net.vasak.os.Gamification",
    default_path = "/ar/net/vasak/os/Gamification",
    gen_blocking = false
)]
pub trait Gamification {
    /// Un módulo avisa que pasó algo puntuable de tipo `kind` (p. ej. `"break"`,
    /// `"hydration"`). El emisor no manda puntos: cuánto suma ese `kind` lo
    /// decide el servicio. Apagada la capa, no hace nada.
    fn emit(&self, kind: &str) -> zbus::Result<()>;

    /// El progreso: XP, racha y logros desbloqueados. Apagada la capa, `enabled`
    /// es `false` y el resto viene en cero.
    fn progress(&self) -> zbus::Result<GamificationProgress>;

    /// Vacía el progreso: XP, racha y logros vuelven a cero.
    fn reset(&self) -> zbus::Result<()>;

    /// Prende o apaga la capa en el acto, sin reiniciar. Quien lo llame también
    /// debería persistir `gamification.enabled` en `vasak.conf`, que es de donde
    /// el servicio y el emisor lo leen al arrancar.
    fn set_enabled(&self, enabled: bool) -> zbus::Result<()>;

    /// Si la capa está prendida ahora mismo.
    fn enabled(&self) -> zbus::Result<bool>;
}
