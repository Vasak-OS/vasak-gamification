//! La interfaz D-Bus: lo que un módulo emisor y Configuración pueden pedirle.
//!
//! Los nombres de los métodos en el bus son los mismos de Rust en PascalCase
//! (`emit` → `Emit`), y están declarados una sola vez, igual, en el proxy del
//! crate `vasak-gamification-protocol`.
//!
//! El servicio **dueña el estado en memoria** (`Arc<Mutex<Progress>>`): cada
//! `Emit` lo actualiza y lo guarda en el acto, de forma atómica, bajo el lock.
//! El interruptor `enabled` se lee una vez al arrancar; hoy no cambia en
//! caliente (mismo límite conocido que tenía la capa dentro de health).

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tracing::warn;
use zbus::interface;

use vasak_gamification_protocol::GamificationProgress;

use crate::gamification::{self, award, points_for, Event, Progress};

pub struct GamificationService {
    /// El progreso, en memoria y compartido con `main` (que lo guarda al salir).
    pub progress: Arc<Mutex<Progress>>,
    /// Si la capa está prendida. Se lee una vez al arrancar.
    pub enabled: bool,
    /// Dónde vive el estado; `None` sin carpeta de datos.
    pub dir: Option<PathBuf>,
}

#[interface(name = "ar.net.vasak.os.Gamification")]
impl GamificationService {
    /// Un módulo avisa que pasó algo puntuable de tipo `kind`. El servicio —no
    /// el emisor— decide cuántos puntos vale con [`points_for`], arma el evento
    /// y se lo pasa al motor. Apagada la capa, no hace nada.
    ///
    /// La IO es corta y va bajo el lock: `save` escribe un archivo chico de
    /// forma atómica. No hay `.await` sostenido, así que no se bloquea el
    /// runtime más de lo que tarda ese renombre.
    fn emit(&self, kind: &str) {
        if !self.enabled {
            return;
        }
        // `kind` llega por el bus de sesión desde cualquier proceso: uno vacío o
        // desmesurado es un emisor roto o malicioso y no se puntúa. Corta en el
        // borde el `kind` gigante antes de que entre a memoria o disco; la cota de
        // cardinalidad vive en `award`.
        if kind.is_empty() || kind.len() > gamification::MAX_KIND_LEN {
            return;
        }
        let event = Event {
            kind: kind.to_string(),
            points: points_for(kind),
        };
        let mut progress = self.progress.lock();
        award(&mut progress, &event, chrono::Local::now().date_naive());
        self.persist(&progress);
    }

    /// El progreso: XP, racha y logros. Apagada la capa, `enabled` es `false` y
    /// el resto viene en cero.
    fn progress(&self) -> GamificationProgress {
        self.progress.lock().report(self.enabled)
    }

    /// Vacía el progreso: XP, racha y logros vuelven a cero. Se guarda el estado
    /// en cero aunque la capa esté apagada, para que un reset quede firme.
    fn reset(&self) {
        let mut progress = self.progress.lock();
        *progress = Progress::default();
        self.persist(&progress);
    }
}

impl GamificationService {
    /// Guarda el progreso si hay carpeta de datos; un error se registra y no
    /// voltea el servicio (lo peor es perder el último evento, no la historia).
    fn persist(&self, progress: &Progress) {
        if let Some(dir) = &self.dir {
            if let Err(error) = gamification::save(dir, progress) {
                warn!("no se pudo guardar el progreso de gamificación: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::TempDir;

    fn servicio(enabled: bool, dir: &TempDir) -> GamificationService {
        GamificationService {
            progress: Arc::new(Mutex::new(Progress::default())),
            enabled,
            dir: Some(dir.0.clone()),
        }
    }

    #[test]
    fn apagado_emit_no_suma_ni_guarda() {
        let temp = TempDir::new("servicio-apagado");
        let service = servicio(false, &temp);
        service.emit("break");
        assert_eq!(service.progress.lock().xp, 0);
        assert!(!temp.0.join("progress.json").exists());
    }

    #[test]
    fn prendido_emit_suma_puntos_desbloquea_y_persiste() {
        let temp = TempDir::new("servicio-prendido");
        let service = servicio(true, &temp);
        service.emit("break");
        let informe = service.progress();
        assert!(informe.enabled);
        assert_eq!(informe.xp, u64::from(points_for("break")));
        assert_eq!(informe.streak_days, 1);
        assert!(!informe.achievements.is_empty());
        // Lo escrito en disco es lo mismo que hay en memoria.
        assert_eq!(gamification::load(&temp.0).xp, informe.xp);
    }

    #[test]
    fn apagado_progress_responde_en_cero_sin_enabled() {
        let temp = TempDir::new("servicio-apagado-informe");
        let service = servicio(false, &temp);
        let informe = service.progress();
        assert!(!informe.enabled);
        assert_eq!((informe.xp, informe.streak_days), (0, 0));
        assert!(informe.achievements.is_empty());
    }

    #[test]
    fn reset_vacia_el_progreso_y_lo_guarda() {
        let temp = TempDir::new("servicio-reset");
        let service = servicio(true, &temp);
        service.emit("break");
        service.reset();
        let informe = service.progress();
        assert_eq!((informe.xp, informe.streak_days), (0, 0));
        assert!(informe.achievements.is_empty());
        assert_eq!(gamification::load(&temp.0), Progress::default());
        assert!(temp.0.join("progress.json").exists());
    }

    #[test]
    fn sin_carpeta_de_datos_emit_sigue_sumando_en_memoria() {
        let service = GamificationService {
            progress: Arc::new(Mutex::new(Progress::default())),
            enabled: true,
            dir: None,
        };
        service.emit("hydration");
        assert_eq!(service.progress().xp, 10);
    }

    #[test]
    fn emit_descarta_kind_vacio_o_demasiado_largo_y_acepta_el_tope() {
        let max = gamification::MAX_KIND_LEN;
        for kind in [String::new(), "x".repeat(max + 1)] {
            let temp = TempDir::new("servicio-kind-invalido");
            let service = servicio(true, &temp);
            service.emit(&kind);
            assert_eq!(
                service.progress.lock().xp,
                0,
                "kind de {} bytes",
                kind.len()
            );
            assert!(!temp.0.join("progress.json").exists());
        }

        let temp = TempDir::new("servicio-kind-tope");
        let service = servicio(true, &temp);
        service.emit(&"x".repeat(max));
        assert_eq!(service.progress.lock().xp, 10);
    }
}
