//! Dónde está `vasak.conf`.
//!
//! El interruptor de la capa lo escriben Configuración y el escritorio en la
//! misma `vasak.conf` que el resto del sistema, con la clave
//! `gamification.enabled`. El servicio la lee al arrancar (la lectura vive en
//! `gamification.rs`); acá sólo queda de dónde sale la ruta del archivo.

use std::path::PathBuf;

/// La ruta de `vasak.conf`. `VASAK_CONFIG_PATH` la pisa, como en el resto del
/// sistema.
pub fn config_path() -> Option<PathBuf> {
    if let Some(set) = std::env::var_os("VASAK_CONFIG_PATH") {
        if !set.is_empty() {
            return Some(PathBuf::from(set));
        }
    }
    dirs::home_dir().map(|home| home.join(".config/vasak/vasak.conf"))
}

/// Candado para las pruebas que pisan `VASAK_CONFIG_PATH`: la variable es del
/// proceso entero y las pruebas corren en paralelo, así que las que la tocan lo
/// toman primero para no leerse el archivo equivocado entre sí.
#[cfg(test)]
pub(crate) static CONFIG_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
