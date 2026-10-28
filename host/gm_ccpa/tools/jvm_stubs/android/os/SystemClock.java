// RUNTIME-ONLY stub for the tier-0 JVM unit tests (tools/test.sh, tools/jvm_tests.txt). Never on a
// compile classpath (tests compile against android.jar) and never packaged. Copied verbatim from the
// v5.7 U3 harness (it_v57_U3.sh), whose SessionSupervisorTest reads Log.LINES by reflection;
// SystemClock throws so a test that forgets to inject its clock fails loudly.
package android.os;
public final class SystemClock {
    public static long elapsedRealtime() { throw new IllegalStateException("host test reached SystemClock: inject the clock"); }
}
