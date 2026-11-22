//! The phone's position. On Android a thread asks for the location
//! permission, keeps the location providers updating through a
//! `PendingIntent` nobody receives (so no Java class is needed) and reads
//! their newest fix once a second. Elsewhere there is no position.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{LazyLock, Mutex};

use dfui::Notifier;

/// Latitude and longitude in degrees, and accuracy in metres.
pub type Fix = (f64, f64, f32);

/// The latest fix and the first since `start`.
static FIX: Mutex<(Option<Fix>, Option<Fix>)> = Mutex::new((None, None));
static ON: AtomicBool = AtomicBool::new(false);
/// When locating started, ms since the epoch; older fixes are ignored.
static SINCE: AtomicI64 = AtomicI64::new(0);
/// How old a fix the system cached may be and still count, ms.
const STALE_MS: i64 = 120_000;
/// Notified on each new fix.
pub static CHANGED: LazyLock<Notifier> = LazyLock::new(Notifier::new);

/// Whether this build can locate the device.
pub const AVAILABLE: bool = cfg!(target_os = "android");

/// The newest fix since `start`.
pub fn fix() -> Option<Fix> {
    FIX.lock().unwrap().0
}

/// The first fix since `start`.
pub fn first() -> Option<Fix> {
    FIX.lock().unwrap().1
}

/// Starts locating afresh, asking for the permission if needed.
pub fn start() {
    *FIX.lock().unwrap() = (None, None);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let now = now.map_or(0, |d| d.as_millis() as i64);
    SINCE.store(now - STALE_MS, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
    #[cfg(target_os = "android")]
    android::spawn();
}

/// Stops the providers; the last fix stays readable.
pub fn stop() {
    ON.store(false, Ordering::Relaxed);
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn set(f: Fix) {
    let mut fix = FIX.lock().unwrap();
    if fix.0 != Some(f) {
        *fix = (Some(f), fix.1.or(Some(f)));
        drop(fix);
        CHANGED.notify();
    }
}

#[cfg(target_os = "android")]
pub use android::APP;

#[cfg(target_os = "android")]
mod android {
    use std::sync::{Once, OnceLock};
    use std::time::Duration;

    use dfui::AndroidApp;
    use dfui::android::with_activity;
    use jni::objects::{JObject, JValue};
    use jni::{Env, errors::Result, jni_sig, jni_str};

    use super::{Fix, ON, SINCE, set};

    pub static APP: OnceLock<AndroidApp> = OnceLock::new();

    const FINE: &str = "android.permission.ACCESS_FINE_LOCATION";
    const COARSE: &str = "android.permission.ACCESS_COARSE_LOCATION";
    const PROVIDERS: [&str; 3] = ["fused", "gps", "network"];

    pub fn spawn() {
        static THREAD: Once = Once::new();
        THREAD.call_once(|| {
            std::thread::spawn(run);
        });
    }

    // ponytail: polls once a second while on; a LocationListener in Java if
    // fixes need to arrive sooner.
    fn run() {
        let Some(app) = APP.get() else { return };
        let (mut asked, mut listening) = (false, false);
        loop {
            let on = ON.load(std::sync::atomic::Ordering::Relaxed);
            let r = with_activity(app, |env, activity| {
                let r = step(env, activity, app, on, &mut asked, &mut listening);
                if r.is_err() {
                    // Logs the Java exception before `with_activity` clears it.
                    env.exception_describe();
                }
                r
            });
            match r {
                Ok(Some(f)) => set(f),
                Ok(None) => {}
                Err(e) => log::warn!("locating failed: {e}"),
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// One look: asks for the permission once, starts or stops updates to
    /// match `on`, and reads the newest fix while on.
    fn step(
        env: &mut Env,
        activity: &JObject,
        app: &AndroidApp,
        on: bool,
        asked: &mut bool,
        listening: &mut bool,
    ) -> Result<Option<Fix>> {
        if !granted(env, activity)? {
            if on && !*asked {
                *asked = true;
                request_permission(app);
            }
            return Ok(None);
        }
        let service = env.new_string("location")?;
        let manager = env
            .call_method(
                activity,
                jni_str!("getSystemService"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[JValue::Object(&service)],
            )?
            .l()?;
        let intent = pending(env, activity)?;
        if on != *listening {
            updates(env, &manager, &intent, on)?;
            *listening = on;
        }
        if on { newest(env, &manager) } else { Ok(None) }
    }

    fn granted(env: &mut Env, activity: &JObject) -> Result<bool> {
        for p in [FINE, COARSE] {
            let p = env.new_string(p)?;
            let r = env
                .call_method(
                    activity,
                    jni_str!("checkSelfPermission"),
                    jni_sig!("(Ljava/lang/String;)I"),
                    &[JValue::Object(&p)],
                )?
                .i()?;
            if r == 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Shows the system's permission dialog; the next checks see the answer.
    fn request_permission(app: &AndroidApp) {
        let a = app.clone();
        app.run_on_java_main_thread(Box::new(move || {
            let r = with_activity(&a, |env, activity| {
                let fine = env.new_string(FINE)?;
                let both = env.new_object_array(2, jni_str!("java/lang/String"), &fine)?;
                let coarse = env.new_string(COARSE)?;
                both.set_element(env, 1, &coarse)?;
                env.call_method(
                    activity,
                    jni_str!("requestPermissions"),
                    jni_sig!("([Ljava/lang/String;I)V"),
                    &[JValue::Object(&both), JValue::Int(1)],
                )?;
                Ok(())
            });
            if let Err(e) = r {
                log::warn!("asking for the location permission failed: {e}");
            }
        }));
    }

    /// The same `PendingIntent` each call, so updates can be removed again.
    fn pending<'a>(env: &mut Env<'a>, activity: &JObject) -> Result<JObject<'a>> {
        // FLAG_MUTABLE | FLAG_UPDATE_CURRENT: location updates need a
        // mutable one, which Android 14 only allows for an explicit intent.
        const FLAGS: i32 = 0x0200_0000 | 0x0800_0000;
        let action = env.new_string("at.busapp.innsbruck.LOCATION")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&action)],
        )?;
        let package = env
            .call_method(
                activity,
                jni_str!("getPackageName"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        env.call_method(
            &intent,
            jni_str!("setPackage"),
            jni_sig!("(Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(&package)],
        )?;
        env.call_static_method(
            jni_str!("android/app/PendingIntent"),
            jni_str!("getBroadcast"),
            jni_sig!(
                "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;"
            ),
            &[
                JValue::Object(activity),
                JValue::Int(0),
                JValue::Object(&intent),
                JValue::Int(FLAGS),
            ],
        )?
        .l()
    }

    /// Starts or stops updates from every enabled provider.
    fn updates(env: &mut Env, manager: &JObject, intent: &JObject, on: bool) -> Result<()> {
        if !on {
            env.call_method(
                manager,
                jni_str!("removeUpdates"),
                jni_sig!("(Landroid/app/PendingIntent;)V"),
                &[JValue::Object(intent)],
            )?;
            return Ok(());
        }
        for p in PROVIDERS {
            let name = env.new_string(p)?;
            if enabled(env, manager, &name)? {
                env.call_method(
                    manager,
                    jni_str!("requestLocationUpdates"),
                    jni_sig!("(Ljava/lang/String;JFLandroid/app/PendingIntent;)V"),
                    &[
                        JValue::Object(&name),
                        JValue::Long(1000),
                        JValue::Float(0.0),
                        JValue::Object(intent),
                    ],
                )?;
            }
        }
        Ok(())
    }

    /// Whether the provider exists and is on; asking one that doesn't
    /// exist for anything else throws.
    fn enabled(env: &mut Env, manager: &JObject, name: &JObject) -> Result<bool> {
        env.call_method(
            manager,
            jni_str!("isProviderEnabled"),
            jni_sig!("(Ljava/lang/String;)Z"),
            &[JValue::Object(name)],
        )?
        .z()
    }

    /// The most recent fix any provider has.
    fn newest(env: &mut Env, manager: &JObject) -> Result<Option<Fix>> {
        let mut best: Option<(i64, Fix)> = None;
        for p in PROVIDERS {
            let name = env.new_string(p)?;
            if !enabled(env, manager, &name)? {
                continue;
            }
            let loc = env
                .call_method(
                    manager,
                    jni_str!("getLastKnownLocation"),
                    jni_sig!("(Ljava/lang/String;)Landroid/location/Location;"),
                    &[JValue::Object(&name)],
                )?
                .l()?;
            if loc.is_null() {
                continue;
            }
            let mut get = |m, sig| env.call_method(&loc, m, sig, &[]);
            let time = get(jni_str!("getTime"), jni_sig!("()J"))?.j()?;
            let lat = get(jni_str!("getLatitude"), jni_sig!("()D"))?.d()?;
            let lon = get(jni_str!("getLongitude"), jni_sig!("()D"))?.d()?;
            let acc = get(jni_str!("getAccuracy"), jni_sig!("()F"))?.f()?;
            let fresh = time >= SINCE.load(std::sync::atomic::Ordering::Relaxed);
            if fresh && best.is_none_or(|(t, _)| time > t) {
                best = Some((time, (lat, lon, acc)));
            }
        }
        Ok(best.map(|(_, f)| f))
    }
}
