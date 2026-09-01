package android.car.projection;

import java.util.List;

/**
 * COMPILE-ONLY STUB of AAOS's {@code @SystemApi android.car.projection.ProjectionStatus}.
 *
 * See {@link android.car.CarProjectionManager} for why these stubs exist and how they were
 * obtained. This file is never packaged; the platform's real class is bound at runtime.
 *
 * <h2>The integer values are load-bearing</h2>
 *
 * {@code static final int} constants are <b>inlined by javac into our bytecode</b>, so a wrong
 * value here is not caught by anything — it silently publishes the wrong projection state forever.
 * Every value below was read from the Pi's own {@code /system/framework/android.car.jar}
 * (AAOS 16, build 2026-04-13) rather than recalled:
 *
 * <pre>
 *   PROJECTION_STATE_INACTIVE          = 0
 *   PROJECTION_STATE_READY_TO_PROJECT  = 1
 *   PROJECTION_STATE_ACTIVE_FOREGROUND = 2
 *   PROJECTION_STATE_ACTIVE_BACKGROUND = 3
 *   PROJECTION_STATE_ATTEMPTING        = 4
 *   PROJECTION_STATE_FINISHING         = 5
 *   PROJECTION_TRANSPORT_NONE = 0, USB = 1, WIFI = 2
 * </pre>
 *
 * The state vocabulary maps cleanly onto this project's session model, which is worth noting
 * because it confirms the framework was designed for exactly the session-vs-foreground split
 * {@code pi/docs/01_PROJECTION_APP_DESIGN.md} §1 settled on: a projection app is expected to stay
 * ACTIVE while backgrounded, and AAOS has a distinct state to say so.
 */
public class ProjectionStatus {

    public static final int PROJECTION_STATE_INACTIVE = 0;
    public static final int PROJECTION_STATE_READY_TO_PROJECT = 1;
    public static final int PROJECTION_STATE_ACTIVE_FOREGROUND = 2;
    public static final int PROJECTION_STATE_ACTIVE_BACKGROUND = 3;
    public static final int PROJECTION_STATE_ATTEMPTING = 4;
    public static final int PROJECTION_STATE_FINISHING = 5;

    public static final int PROJECTION_TRANSPORT_NONE = 0;
    public static final int PROJECTION_TRANSPORT_USB = 1;
    public static final int PROJECTION_TRANSPORT_WIFI = 2;

    ProjectionStatus() {
    }

    public static Builder builder(String packageName, int state) {
        throw new UnsupportedOperationException("stub");
    }

    public String getPackageName() {
        throw new UnsupportedOperationException("stub");
    }

    public int getState() {
        throw new UnsupportedOperationException("stub");
    }

    public int getTransport() {
        throw new UnsupportedOperationException("stub");
    }

    public boolean isActive() {
        throw new UnsupportedOperationException("stub");
    }

    public List<MobileDevice> getConnectedMobileDevices() {
        throw new UnsupportedOperationException("stub");
    }

    public static final class Builder {
        Builder() {
        }

        public Builder setProjectionTransport(int transport) {
            throw new UnsupportedOperationException("stub");
        }

        public Builder addMobileDevice(MobileDevice device) {
            throw new UnsupportedOperationException("stub");
        }

        public ProjectionStatus build() {
            throw new UnsupportedOperationException("stub");
        }
    }

    /**
     * One phone known to the projection app. {@code id} is the app's own stable identifier for the
     * device — AAOS treats it as opaque.
     */
    public static final class MobileDevice {
        MobileDevice() {
        }

        public static Builder builder(int id, String name) {
            throw new UnsupportedOperationException("stub");
        }

        public int getId() {
            throw new UnsupportedOperationException("stub");
        }

        public String getName() {
            throw new UnsupportedOperationException("stub");
        }

        public boolean isProjecting() {
            throw new UnsupportedOperationException("stub");
        }

        public List<Integer> getAvailableTransports() {
            throw new UnsupportedOperationException("stub");
        }

        public static final class Builder {
            Builder() {
            }

            public Builder setProjecting(boolean projecting) {
                throw new UnsupportedOperationException("stub");
            }

            public Builder addTransport(int transport) {
                throw new UnsupportedOperationException("stub");
            }

            public MobileDevice build() {
                throw new UnsupportedOperationException("stub");
            }
        }
    }
}
