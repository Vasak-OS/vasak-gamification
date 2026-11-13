//! El motor de la gamificación.
//!
//! Apagada por omisión. Prendida, cualquier módulo del sistema puede emitir un
//! [`Event`] por D-Bus y el motor decide XP, logros y rachas. El estado vive en
//! texto plano, en el directorio de datos del usuario (no es secreto; nada de
//! llavero).
//!
//! El motor es puro: [`award`] toma el día como parámetro y no toca disco ni
//! reloj, así se prueba sin esperar un cambio de fecha. El acople con el disco
//! ([`load`]/[`save`]) y con el bus (en `service.rs`) es la única parte que mira
//! el sistema de archivos.
//!
//! El emisor **no manda puntos**: cuánto vale cada tipo de evento lo decide el
//! servicio, con [`points_for`]. Así la economía de puntos se afina en un solo
//! lugar sin recompilar a quien emite.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use vasak_gamification_protocol::GamificationProgress;

use crate::config;
use crate::store::write_atomically;

/// Nombre del archivo de estado, dentro de `dirs::data_dir()/vasak-gamification`.
/// Texto plano.
const STATE_FILE: &str = "progress.json";

/// Umbrales de XP que desbloquean un logro, con su id estable. Son un **default
/// abierto**: los números y la cantidad de hitos pueden cambiarse sin tocar la
/// lógica, y los ids se mantienen estables para no re-desbloquear lo ya ganado.
const XP_ACHIEVEMENTS: &[(u64, &str)] = &[(100, "xp-100"), (500, "xp-500"), (1000, "xp-1000")];

/// Largo máximo de un `kind`, en bytes. `Emit` llega por el bus de sesión desde
/// cualquier proceso: un `kind` desmesurado es un emisor roto o malicioso y se
/// descarta en el borde (`service::emit`), para que no entre a disco ni a memoria.
pub const MAX_KIND_LEN: usize = 64;

/// Tope de tipos distintos que se registran. Cada `kind` nuevo mina un logro
/// `first-<kind>` y entra a `seen_kinds`; sin cota, un emisor podría hacer crecer
/// el estado sin límite. Llegado el tope se sigue sumando XP, pero no se crean
/// logros ni se registran tipos nuevos. Es un **default abierto**.
const MAX_SEEN_KINDS: usize = 256;

/// Tamaño máximo del archivo de la versión vieja que se acepta importar. Un
/// progreso legítimo son unos pocos KiB; el tope evita un OOM si alguien planta
/// un archivo enorme en la ruta vieja, que es escribible por la sesión.
const MAX_LEGACY_BYTES: u64 = 1 << 20; // 1 MiB

/// Los puntos que vale cada tipo de evento. **El servicio —no el emisor— decide
/// cuánto suma cada `kind`**: por eso vive acá y no viaja por D-Bus. Los kinds
/// conocidos tienen su valor; cualquier otro cae en el **default abierto**, así
/// un módulo nuevo puede empezar a puntuar sin tocar esta tabla y después se le
/// ajusta el valor si hace falta.
pub fn points_for(kind: &str) -> u32 {
    match kind {
        "break" => 10,
        "hydration" => 10,
        // Default abierto: un kind todavía sin valor propio suma 10.
        _ => 10,
    }
}

/// La configuración de la gamificación, leída de `vasak.conf`.
#[derive(Debug, Clone, PartialEq)]
pub struct GamificationConfig {
    pub enabled: bool,
}

/// Lee la sección `gamification` de `vasak.conf`: `enabled` (apagada si falta o
/// no es un booleano). Un archivo ilegible o sin la sección deja la capa
/// apagada.
pub fn gamification_from_json(content: &str) -> GamificationConfig {
    let enabled = serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .as_ref()
        .and_then(|cfg| cfg.get("gamification"))
        .and_then(|section| section.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    GamificationConfig { enabled }
}

/// Lee `vasak.conf` y devuelve la configuración de la gamificación; apagada si
/// el archivo no se puede leer.
pub fn read_gamification_config() -> GamificationConfig {
    config::config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|content| gamification_from_json(&content))
        .unwrap_or(GamificationConfig { enabled: false })
}

/// Lo que emite un módulo para que la capa lo puntúe. Genérico a propósito: la
/// capa no se ata a ningún módulo en particular. `kind` identifica de qué se
/// trata (p. ej. `"break"`, `"hydration"`) y `points` cuánto suma —lo decide el
/// servicio con [`points_for`], no el emisor—.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: String,
    pub points: u32,
}

/// El progreso acumulado de la persona. Se guarda y se lee tal cual de disco.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    /// Experiencia total acumulada.
    pub xp: u64,
    /// Días seguidos con al menos un evento.
    pub streak_days: u32,
    /// Último día con actividad, para decidir si la racha sigue o se corta.
    pub last_active_day: Option<NaiveDate>,
    /// Ids de los logros ya desbloqueados, sin repetir.
    pub achievements: Vec<String>,
    /// `kind`s ya vistos, para el logro de «primer evento de cada tipo».
    pub seen_kinds: BTreeSet<String>,
}

impl Progress {
    /// Arma el informe que viaja por D-Bus: XP, racha y logros, con el
    /// interruptor `enabled` que decide quien consulta. Los campos de
    /// contabilidad interna (`last_active_day`, `seen_kinds`) no salen: a quien
    /// muestra el progreso no le sirven.
    pub fn report(&self, enabled: bool) -> GamificationProgress {
        GamificationProgress {
            enabled,
            xp: self.xp,
            streak_days: self.streak_days,
            achievements: self.achievements.clone(),
        }
    }
}

/// El corazón, puro: aplica `event` a `progress` con `today` como el día actual.
/// Suma la XP, actualiza la racha y desbloquea los logros que correspondan. No
/// toca disco ni reloj; el día entra por parámetro para poder probarla.
pub fn award(progress: &mut Progress, event: &Event, today: NaiveDate) {
    progress.xp += u64::from(event.points);
    update_streak(progress, today);

    // Logro de «primer evento de cada kind»: si el tipo entra por primera vez a
    // `seen_kinds`, se desbloquea su logro. `insert` devuelve `true` sólo la
    // primera vez, así no se repite. Se corta en `MAX_SEEN_KINDS` para que un
    // emisor no haga crecer el estado sin límite: pasado el tope la XP sigue
    // sumando, pero no se registran tipos ni logros nuevos.
    if progress.seen_kinds.len() < MAX_SEEN_KINDS && progress.seen_kinds.insert(event.kind.clone())
    {
        unlock(progress, format!("first-{}", event.kind));
    }

    // Logros por umbral de XP.
    for (threshold, id) in XP_ACHIEVEMENTS {
        if progress.xp >= *threshold {
            unlock(progress, (*id).to_string());
        }
    }
}

/// Actualiza la racha con el día `today`:
/// - mismo día que el último → la racha no cambia (varios eventos en un día
///   cuentan como uno);
/// - el día siguiente al último → la racha crece en uno;
/// - un hueco, un salto hacia atrás, o el primer evento → la racha vuelve a 1.
fn update_streak(progress: &mut Progress, today: NaiveDate) {
    progress.streak_days = match progress.last_active_day {
        Some(last) if last == today => progress.streak_days,
        Some(last) if last.succ_opt() == Some(today) => progress.streak_days + 1,
        _ => 1,
    };
    progress.last_active_day = Some(today);
}

/// Desbloquea un logro si no estaba ya: un logro no se duplica.
fn unlock(progress: &mut Progress, id: String) {
    if !progress.achievements.contains(&id) {
        progress.achievements.push(id);
    }
}

/// El directorio base del estado: `dirs::data_dir()/vasak-gamification`.
pub fn default_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|data| data.join("vasak-gamification"))
}

/// El archivo de estado de la versión vieja, cuando la capa vivía dentro de
/// `vasak-health-service`. De ahí se importa una sola vez.
pub fn legacy_file() -> Option<PathBuf> {
    dirs::data_dir().map(|data| data.join("vasak-health-service").join("gamification.json"))
}

/// Lee el progreso guardado en `dir`. Si no hay archivo o no parsea, arranca de
/// cero (`Progress::default()`): un estado roto no tiene que voltear el demonio.
pub fn load(dir: &Path) -> Progress {
    std::fs::read_to_string(dir.join(STATE_FILE))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

/// Guarda el progreso en `dir`, de forma atómica.
pub fn save(dir: &Path, progress: &Progress) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let body = serde_json::to_vec_pretty(progress)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomically(&dir.join(STATE_FILE), &body)
}

/// Si `dir` todavía no tiene estado, trae una sola vez el progreso que
/// `vasak-health-service` haya dejado en su archivo viejo, para no perder lo de
/// quien prendió la capa cuando vivía ahí. Devuelve `true` si importó algo.
///
/// La decisión de «vacío» mira que el archivo nuevo no exista, no que el
/// progreso leído sea cero: así un reset legítimo (todo en cero) no vuelve a
/// disparar la importación. Un archivo viejo ilegible se ignora y se arranca de
/// cero, igual que `load`.
pub fn import_legacy_if_empty(dir: &Path) -> io::Result<bool> {
    let Some(legacy) = legacy_file() else {
        return Ok(false);
    };
    import_legacy_from(dir, &legacy)
}

/// El núcleo de [`import_legacy_if_empty`], con la ruta vieja inyectada para
/// poder probarlo sin tocar el `HOME` real: importa el `legacy` a `dir` sólo si
/// `dir` todavía no tiene estado, y sólo si el viejo parsea.
pub fn import_legacy_from(dir: &Path, legacy: &Path) -> io::Result<bool> {
    if dir.join(STATE_FILE).exists() {
        return Ok(false);
    }
    // El archivo viejo es escribible por cualquier proceso de la sesión: si
    // alguien planta ahí un archivo enorme, cargarlo entero al arrancar sería un
    // OOM. Un progreso legítimo son unos pocos KiB; por encima del tope se ignora
    // igual que uno ilegible.
    match std::fs::metadata(legacy) {
        Ok(meta) if meta.len() > MAX_LEGACY_BYTES => return Ok(false),
        Ok(_) => {}
        Err(_) => return Ok(false),
    }
    let Ok(content) = std::fs::read_to_string(legacy) else {
        return Ok(false);
    };
    let Ok(progress) = serde_json::from_str::<Progress>(&content) else {
        return Ok(false);
    };
    save(dir, &progress)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::TempDir;

    fn day(n: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, n).expect("fecha válida")
    }

    fn event(kind: &str, points: u32) -> Event {
        Event {
            kind: kind.to_string(),
            points,
        }
    }

    #[test]
    fn sin_configuracion_la_gamificacion_queda_apagada() {
        for content in [
            "{}",
            "no es json",
            "",
            r#"{"gamification":{}}"#,
            r#"{"gamification":{"enabled":"true"}}"#,
            r#"{"gamification":{"enabled":1}}"#,
        ] {
            assert!(!gamification_from_json(content).enabled, "{content}");
        }
    }

    #[test]
    fn prendida_sólo_si_la_clave_dice_true() {
        assert!(gamification_from_json(r#"{"gamification":{"enabled":true}}"#).enabled);
        assert!(!gamification_from_json(r#"{"gamification":{"enabled":false}}"#).enabled);
    }

    #[test]
    fn la_configuracion_se_lee_de_la_ruta_de_vasak_conf() {
        // `VASAK_CONFIG_PATH` es del proceso: serializamos con cualquier otra
        // prueba que la pise (ver `config::CONFIG_ENV_LOCK`).
        let _env = crate::config::CONFIG_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new("gamificacion-conf");
        let file = temp.0.join("vasak.conf");
        std::fs::write(&file, r#"{"gamification":{"enabled":true}}"#)
            .expect("se escribe la configuración");

        std::env::set_var("VASAK_CONFIG_PATH", &file);
        let leida = read_gamification_config();
        std::env::set_var("VASAK_CONFIG_PATH", temp.0.join("no-existe.conf"));
        let ausente = read_gamification_config();
        std::env::remove_var("VASAK_CONFIG_PATH");

        assert!(leida.enabled);
        assert!(!ausente.enabled);
    }

    #[test]
    fn la_xp_suma_los_puntos_y_acumula_entre_eventos() {
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 10), day(1));
        assert_eq!(progress.xp, 10);
        award(&mut progress, &event("break", 15), day(1));
        award(&mut progress, &event("hydration", 5), day(1));
        assert_eq!(progress.xp, 30);
    }

    #[test]
    fn el_primer_evento_arranca_la_racha_en_uno() {
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 1), day(5));
        assert_eq!(progress.streak_days, 1);
        assert_eq!(progress.last_active_day, Some(day(5)));
    }

    #[test]
    fn el_dia_siguiente_suma_a_la_racha() {
        // Distingue de la ingenua «siempre resetea»: acá tiene que llegar a 3.
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 1), day(1));
        award(&mut progress, &event("break", 1), day(2));
        assert_eq!(progress.streak_days, 2);
        award(&mut progress, &event("break", 1), day(3));
        assert_eq!(progress.streak_days, 3);
        assert_eq!(progress.last_active_day, Some(day(3)));
    }

    #[test]
    fn varios_eventos_el_mismo_dia_no_inflan_la_racha() {
        // Distingue de la ingenua «siempre +1»: el mismo día no cuenta dos veces.
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 1), day(1));
        award(&mut progress, &event("break", 1), day(2));
        award(&mut progress, &event("break", 1), day(2));
        award(&mut progress, &event("break", 1), day(2));
        assert_eq!(progress.streak_days, 2);
        assert_eq!(progress.last_active_day, Some(day(2)));
    }

    #[test]
    fn un_hueco_de_dias_reinicia_la_racha() {
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 1), day(1));
        award(&mut progress, &event("break", 1), day(2));
        award(&mut progress, &event("break", 1), day(4));
        assert_eq!(progress.streak_days, 1);
        assert_eq!(progress.last_active_day, Some(day(4)));
    }

    #[test]
    fn una_fecha_hacia_atras_reinicia_la_racha() {
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 1), day(10));
        award(&mut progress, &event("break", 1), day(11));
        award(&mut progress, &event("break", 1), day(3));
        assert_eq!(progress.streak_days, 1);
        assert_eq!(progress.last_active_day, Some(day(3)));
    }

    #[test]
    fn los_umbrales_de_xp_desbloquean_su_logro_al_cruzarlos() {
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 99), day(1));
        assert!(!progress.achievements.contains(&"xp-100".to_string()));
        award(&mut progress, &event("break", 1), day(1));
        assert!(progress.achievements.contains(&"xp-100".to_string()));
        assert!(!progress.achievements.contains(&"xp-500".to_string()));

        award(&mut progress, &event("break", 900), day(1));
        for id in ["xp-500", "xp-1000"] {
            assert!(progress.achievements.contains(&id.to_string()), "{id}");
        }
    }

    #[test]
    fn un_logro_de_umbral_no_se_duplica() {
        // Distingue de la ingenua «push siempre»: ya con 100 de XP, seguir
        // sumando no vuelve a agregar `xp-100`.
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 100), day(1));
        award(&mut progress, &event("break", 10), day(1));
        award(&mut progress, &event("break", 10), day(2));
        let cuantos = progress
            .achievements
            .iter()
            .filter(|id| *id == "xp-100")
            .count();
        assert_eq!(cuantos, 1);
    }

    #[test]
    fn el_primer_evento_de_cada_tipo_da_su_logro_una_sola_vez() {
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 1), day(1));
        award(&mut progress, &event("break", 1), day(1));
        assert_eq!(progress.achievements, vec!["first-break".to_string()]);

        award(&mut progress, &event("hydration", 1), day(1));
        assert_eq!(
            progress.achievements,
            vec!["first-break".to_string(), "first-hydration".to_string()]
        );
    }

    #[test]
    fn el_progreso_vuelve_igual_de_disco() {
        let temp = TempDir::new("gamificacion-ida-y-vuelta");
        let mut progress = Progress::default();
        award(&mut progress, &event("break", 120), day(1));
        award(&mut progress, &event("hydration", 5), day(2));

        save(&temp.0, &progress).expect("se guarda");
        assert_eq!(load(&temp.0), progress);
    }

    #[test]
    fn sin_archivo_o_con_archivo_ilegible_se_arranca_de_cero() {
        let temp = TempDir::new("gamificacion-vacio");
        assert_eq!(load(&temp.0), Progress::default());

        std::fs::write(temp.0.join(STATE_FILE), "esto no es json").expect("se escribe");
        assert_eq!(load(&temp.0), Progress::default());
    }

    fn progreso_con_datos() -> Progress {
        Progress {
            xp: 120,
            streak_days: 4,
            last_active_day: Some(day(5)),
            achievements: vec!["primer_aviso".to_string(), "racha_3".to_string()],
            seen_kinds: ["reminder".to_string()].into_iter().collect(),
        }
    }

    #[test]
    fn el_informe_copia_xp_racha_y_logros_tal_cual() {
        let informe = progreso_con_datos().report(true);
        assert_eq!(informe.xp, 120);
        assert_eq!(informe.streak_days, 4);
        assert_eq!(informe.achievements, vec!["primer_aviso", "racha_3"]);
    }

    #[test]
    fn el_interruptor_del_informe_sale_del_parametro_y_no_del_contenido() {
        let apagado = progreso_con_datos().report(false);
        assert!(!apagado.enabled);
        assert_eq!(apagado.xp, 120);
        assert_eq!(apagado.streak_days, 4);

        let prendido_vacio = Progress::default().report(true);
        assert!(prendido_vacio.enabled);
        assert_eq!(prendido_vacio.xp, 0);
        assert_eq!(prendido_vacio.streak_days, 0);
        assert!(prendido_vacio.achievements.is_empty());
    }

    #[test]
    fn cada_kind_vale_diez_puntos_y_los_desconocidos_tambien() {
        assert_eq!(points_for("break"), 10);
        assert_eq!(points_for("hydration"), 10);
        assert_eq!(points_for("kind-nuevo"), 10);
    }

    #[test]
    fn si_el_estado_nuevo_ya_existe_no_se_importa_el_viejo() {
        let temp = TempDir::new("gamificacion-sin-reimportar");
        let progress = Progress {
            xp: 7,
            ..Default::default()
        };
        save(&temp.0, &progress).unwrap();
        assert!(!import_legacy_if_empty(&temp.0).unwrap());
        assert_eq!(load(&temp.0).xp, 7);
    }

    fn legacy_progress() -> Progress {
        Progress {
            xp: 640,
            streak_days: 4,
            last_active_day: Some(day(9)),
            achievements: vec!["xp-100".to_string(), "xp-500".to_string()],
            seen_kinds: BTreeSet::from(["break".to_string()]),
        }
    }

    #[test]
    fn el_progreso_viejo_se_importa_y_se_carga_igual() {
        let dir = TempDir::new("gamificacion-importa-dir");
        let old = TempDir::new("gamificacion-importa-viejo");
        let legacy = old.0.join("gamification.json");
        std::fs::write(&legacy, serde_json::to_vec(&legacy_progress()).unwrap()).unwrap();

        assert!(import_legacy_from(&dir.0, &legacy).unwrap());
        let loaded = load(&dir.0);
        assert_eq!(loaded.xp, 640);
        assert_eq!(loaded.streak_days, 4);
        assert_eq!(loaded.last_active_day, Some(day(9)));
        assert_eq!(loaded.achievements, vec!["xp-100", "xp-500"]);
        assert_eq!(loaded.seen_kinds, legacy_progress().seen_kinds);
    }

    #[test]
    fn importar_no_pisa_un_estado_nuevo_que_ya_existe() {
        let dir = TempDir::new("gamificacion-no-pisa-dir");
        let old = TempDir::new("gamificacion-no-pisa-viejo");
        let legacy = old.0.join("gamification.json");
        std::fs::write(&legacy, serde_json::to_vec(&legacy_progress()).unwrap()).unwrap();
        save(
            &dir.0,
            &Progress {
                xp: 3,
                ..Default::default()
            },
        )
        .unwrap();

        assert!(!import_legacy_from(&dir.0, &legacy).unwrap());
        let loaded = load(&dir.0);
        assert_eq!(loaded.xp, 3);
        assert!(loaded.achievements.is_empty());
    }

    #[test]
    fn un_archivo_viejo_ilegible_se_ignora_sin_crear_estado() {
        let dir = TempDir::new("gamificacion-basura-dir");
        let old = TempDir::new("gamificacion-basura-viejo");
        let legacy = old.0.join("gamification.json");
        std::fs::write(&legacy, "esto no es json {{{").unwrap();

        assert!(!import_legacy_from(&dir.0, &legacy).unwrap());
        assert!(!dir.0.join(STATE_FILE).exists());
    }

    #[test]
    fn un_archivo_viejo_inexistente_no_importa_nada() {
        let dir = TempDir::new("gamificacion-sin-viejo-dir");
        let old = TempDir::new("gamificacion-sin-viejo-viejo");

        assert!(!import_legacy_from(&dir.0, &old.0.join("no-existe.json")).unwrap());
        assert!(!dir.0.join(STATE_FILE).exists());
    }

    #[test]
    fn la_cardinalidad_de_tipos_y_logros_first_queda_acotada_pero_la_xp_sigue() {
        let mut progress = Progress::default();
        let total = MAX_SEEN_KINDS + 44;
        for i in 0..total {
            award(&mut progress, &event(&format!("k{i}"), 10), day(1));
        }
        let firsts = |p: &Progress| {
            p.achievements
                .iter()
                .filter(|id| id.starts_with("first-"))
                .count()
        };
        assert_eq!(progress.seen_kinds.len(), MAX_SEEN_KINDS);
        assert_eq!(firsts(&progress), MAX_SEEN_KINDS);
        assert_eq!(progress.xp, total as u64 * 10);

        // Un tipo más allá del tope no agrega logro ni se registra.
        award(&mut progress, &event("extra", 10), day(1));
        assert!(!progress.seen_kinds.contains("extra"));
        assert!(!progress.achievements.contains(&"first-extra".to_string()));
        assert_eq!(firsts(&progress), MAX_SEEN_KINDS);
    }

    #[test]
    fn un_archivo_viejo_valido_pero_de_mas_de_un_mib_se_ignora() {
        let dir = TempDir::new("gamificacion-enorme-dir");
        let old = TempDir::new("gamificacion-enorme-viejo");
        let legacy = old.0.join("gamification.json");
        let big = Progress {
            achievements: (0..100_000).map(|i| format!("logro-{i:06}")).collect(),
            ..Default::default()
        };
        std::fs::write(&legacy, serde_json::to_vec(&big).unwrap()).unwrap();
        assert!(std::fs::metadata(&legacy).unwrap().len() > MAX_LEGACY_BYTES);

        assert!(!import_legacy_from(&dir.0, &legacy).unwrap());
        assert!(!dir.0.join(STATE_FILE).exists());
    }

    #[test]
    fn el_progreso_guardado_queda_con_modo_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("gamificacion-modo");
        save(&dir.0, &Progress::default()).unwrap();
        let mode = std::fs::metadata(dir.0.join(STATE_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
