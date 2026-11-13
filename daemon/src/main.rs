//! El servicio de gamificación de VasakOS.
//!
//! Lleva la cuenta de XP, rachas y logros del sistema. Un módulo que hace algo
//! puntuable sólo avisa de qué tipo fue el evento por D-Bus (`Emit`); el
//! servicio decide cuánto vale cada tipo, acumula y guarda el progreso en texto
//! plano en el directorio de datos del usuario. Configuración lo consulta para
//! mostrarlo.
//!
//! No dibuja nada: Configuración —que ya existe— es la que muestra el progreso.
//! Por eso es un demonio de Rust y no una aplicación de Tauri: un runtime de
//! WebKit residente para un servicio sin ventana es el error ya documentado
//! para otros demonios del sistema.
//!
//! Corre en el bus de sesión. Todo lo que toca —el progreso, su archivo— es de
//! quien tiene la sesión abierta. Apagado por omisión: no cuenta nada hasta que
//! la persona prenda la capa en `vasak.conf`.

mod config;
mod gamification;
mod service;
mod store;

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{error, info, warn};
use zbus::connection;

use gamification::Progress;
use service::GamificationService;
use vasak_gamification_protocol::{SERVICE_NAME, SERVICE_PATH};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // El nombre del crate lo pone el compilador: el binario es
    // `vasak-gamification-service` pero el paquete es `vasak-gamification-daemon`,
    // y escribir cualquiera a mano da un filtro que no engancha ningún objetivo
    // y un servicio que no registra nada.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("{}=info", env!("CARGO_CRATE_NAME")).into()),
        )
        .init();

    // El interruptor y la carpeta del progreso se leen una sola vez al arrancar:
    // hoy no cambian en caliente (mismo límite conocido que tenía la capa dentro
    // de health).
    let enabled = gamification::read_gamification_config().enabled;
    let dir = gamification::default_dir();
    match &dir {
        Some(dir) => import_legacy_if_empty(dir),
        None => error!("sin $XDG_DATA_HOME ni carpeta personal: no se guarda nada"),
    }

    // Prendida, se carga el progreso de disco; apagada, se arranca en cero y no
    // se cuenta nada. El estado lo dueña el servicio en memoria y `main` guarda
    // una copia del `Arc` para persistir al salir.
    let progress = if enabled {
        dir.as_deref().map(gamification::load).unwrap_or_default()
    } else {
        Progress::default()
    };
    let progress = Arc::new(Mutex::new(progress));

    let _connection = connection::Builder::session()?
        .name(SERVICE_NAME)?
        .serve_at(
            SERVICE_PATH,
            GamificationService {
                progress: progress.clone(),
                enabled,
                dir: dir.clone(),
            },
        )?
        .build()
        .await?;
    info!("servicio en {SERVICE_NAME}");

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv() => {},
    }

    // Al salir —systemd lo para, o se cierra la sesión— se guarda el progreso.
    // Cada `Emit` ya guarda en el acto, así que esto es una red de seguridad.
    info!("guardando antes de salir");
    if let Some(dir) = &dir {
        if let Err(error) = gamification::save(dir, &progress.lock()) {
            warn!("no se pudo guardar el progreso al salir: {error}");
        }
    }
    Ok(())
}

/// Si el estado nuevo está vacío, trae una sola vez el progreso que
/// `vasak-health-service` haya dejado en su archivo viejo: así no se pierde lo
/// de quien prendió la capa cuando vivía ahí.
fn import_legacy_if_empty(dir: &std::path::Path) {
    match gamification::import_legacy_if_empty(dir) {
        Ok(true) => info!("progreso de gamificación importado desde vasak-health-service"),
        Ok(false) => {}
        Err(error) => warn!("no se pudo importar el progreso viejo: {error}"),
    }
}
