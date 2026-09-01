package android.car;

import android.car.projection.ProjectionStatus;

import java.util.Set;

/**
 * COMPILE-ONLY STUB of AAOS's {@code @SystemApi} CarProjectionManager.
 *
 * <h2>Why this file exists</h2>
 *
 * The projection framework is the sanctioned integration surface for a projection receiver app
 * (see {@code pi/docs/01_PROJECTION_APP_DESIGN.md} §4), but every one of its members is
 * {@code @SystemApi} and therefore <b>stripped from the public SDK stub</b>: the
 * {@code android.car.jar} shipped in {@code platforms/android-37.0/optional/} contains
 * {@code CarUxRestrictions} but no {@code CarProjectionManager} at all. There is no public
 * artifact that declares it, and the on-device {@code /system/framework/android.car.jar} is a DEX
 * jar, which javac cannot compile against.
 *
 * So this declares the exact shape we call. It is a {@code compileOnly} dependency and is never
 * packaged; at runtime the platform's real class is bound.
 *
 * <h2>Provenance — these signatures were read off the device, not recalled</h2>
 *
 * Extracted with {@code dexdump -d} from the Pi's own
 * {@code /system/framework/android.car.jar} (AAOS 16, build 2026-04-13):
 *
 * <pre>
 *   updateProjectionStatus  (Landroid/car/projection/ProjectionStatus;)V
 *   addKeyEventHandler      (Ljava/util/Set;Landroid/car/CarProjectionManager$ProjectionKeyEventHandler;)V
 *   removeKeyEventHandler   (Landroid/car/CarProjectionManager$ProjectionKeyEventHandler;)V
 * </pre>
 *
 * <p>Only members this app actually calls are declared. {@code registerProjectionRunner} and the
 * {@code Bundle}-carrying extras accessors exist on the real class but are omitted deliberately: an
 * unused stub is pure drift risk — nothing would catch it going stale — and declaring them would
 * drag {@code android.jar} onto this pure-Java module's classpath for no benefit.
 *
 * <b>A descriptor mismatch here fails at runtime, not at build time</b> — the compiler is happy
 * with whatever this file says and the {@code NoSuchMethodError} arrives on the device. Regenerate
 * from the target build's jar rather than editing by hand if the platform is ever updated.
 *
 * Note in particular that {@code updateProjectionStatus} takes a {@link ProjectionStatus}, not the
 * {@code (int, String, Bundle)} triple that a plausible reading of the docs suggests.
 */
public class CarProjectionManager {

    /** Fired when the voice-search / assistant key goes down. */
    public static final int KEY_EVENT_VOICE_SEARCH_KEY_DOWN = 0;

    /** Fired on release of a short press of the voice key. */
    public static final int KEY_EVENT_VOICE_SEARCH_SHORT_PRESS_KEY_UP = 1;

    /** Fired once the voice key has been held long enough to count as a long press. */
    public static final int KEY_EVENT_VOICE_SEARCH_LONG_PRESS_KEY_DOWN = 2;

    /** Fired on release of a long press of the voice key. */
    public static final int KEY_EVENT_VOICE_SEARCH_LONG_PRESS_KEY_UP = 3;

    public static final int KEY_EVENT_CALL_KEY_DOWN = 4;
    public static final int KEY_EVENT_CALL_SHORT_PRESS_KEY_UP = 5;
    public static final int KEY_EVENT_CALL_LONG_PRESS_KEY_DOWN = 6;
    public static final int KEY_EVENT_CALL_LONG_PRESS_KEY_UP = 7;

    /**
     * Registering a handler makes the platform <b>suppress its own default behaviour</b> for these
     * keys, which is why this is preferable to intercepting them anywhere else.
     */
    public interface ProjectionKeyEventHandler {
        void onKeyEvent(int event);
    }

    protected CarProjectionManager() {
        // Never constructed by us — obtained from Car.getCarManager.
    }

    public void addKeyEventHandler(
            Set<Integer> events, ProjectionKeyEventHandler handler) {
        throw new UnsupportedOperationException("stub");
    }

    public void removeKeyEventHandler(ProjectionKeyEventHandler handler) {
        throw new UnsupportedOperationException("stub");
    }

    public void updateProjectionStatus(ProjectionStatus status) {
        throw new UnsupportedOperationException("stub");
    }
}
