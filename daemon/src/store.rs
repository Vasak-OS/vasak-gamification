//! Escritura atómica del estado en disco.
//!
//! Se escribe en un temporal **del mismo directorio**, se fuerza al disco y se
//! renombra encima del de verdad. `rename` dentro de un mismo sistema de
//! archivos es atómico: quien lea ve el archivo viejo entero o el nuevo entero,
//! nunca uno a medias, y un corte de luz en el medio deja el viejo. Un temporal
//! en `/tmp` no serviría: suele ser otro sistema de archivos, y ahí `rename`
//! falla o copia.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Escribe `body` en `path` sin dejar nunca un archivo a medias: ver la
/// documentación del módulo.
pub fn write_atomically(path: &Path, body: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sin directorio"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sin nombre"))?
        .to_string_lossy()
        .into_owned();
    // El nombre del temporal lleva pid + nanos para no ser predecible: con un
    // nombre fijo, otro proceso de la sesión podría plantar ahí un symlink y
    // redirigir la escritura. `create_new` (abajo) es la defensa real —falla si
    // el camino ya existe, anulando el symlink plantado—; el sufijo evita además
    // chocar con un temporal que haya quedado de un corte anterior.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp = dir.join(format!(".{name}.tmp-{}-{nonce}", std::process::id()));

    let result = (|| {
        // `create_new` = `O_EXCL`: falla si el camino ya existe, así no se sigue
        // un symlink ni se trunca un archivo ajeno. Modo 0600: el progreso es del
        // usuario y no hace falta que nadie más lo lea.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(body)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        // El renombre queda en el directorio: sin esto, un corte justo después
        // puede volver al archivo viejo.
        if let Ok(directory) = fs::File::open(dir) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
pub mod tests {
    use std::fs;
    use std::path::PathBuf;

    /// Un directorio propio en el temporal del sistema: **nunca** el estado de
    /// la persona. Se borra al soltarlo.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "vasak-gamification-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&dir).expect("se crea el directorio temporal");
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
