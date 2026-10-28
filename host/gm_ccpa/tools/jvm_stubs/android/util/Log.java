// RUNTIME-ONLY stub for the tier-0 JVM unit tests (tools/test.sh, tools/jvm_tests.txt). Never on a
// compile classpath (tests compile against android.jar) and never packaged. Copied verbatim from the
// v5.7 U3 harness (it_v57_U3.sh), whose SessionSupervisorTest reads Log.LINES by reflection;
// SystemClock throws so a test that forgets to inject its clock fails loudly.
package android.util;
public final class Log {
    public static final int VERBOSE = 2, DEBUG = 3, INFO = 4, WARN = 5, ERROR = 6, ASSERT = 7;
    public static final java.util.List<String> LINES = java.util.Collections.synchronizedList(new java.util.ArrayList<String>());
    public static int println(int priority, String tag, String msg) {
        String l = "VDIWEA".charAt(Math.max(0, Math.min(5, priority - 2))) + " " + tag + " " + msg;
        LINES.add(l);
        if (System.getenv("U3_VERBOSE") != null) System.out.println(l);
        return 0;
    }
}
