import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Objects;

/** Host test runner for the helper cores; no test framework so build.sh needs only the JDK. */
final class Tests {
    interface Case {
        void run() throws Exception;
    }

    private final List<String> failed = new ArrayList<>();
    private int passed;

    void add(String name, Case test) {
        try {
            test.run();
            passed++;
        } catch (Throwable e) {
            failed.add(name);
            System.err.println("FAIL " + name);
            e.printStackTrace();
        }
    }

    static void eq(Object want, Object got) {
        if (!Objects.equals(want, got)) {
            throw new AssertionError("\nwant " + want + "\n got " + got);
        }
    }

    static void check(boolean ok, String what) {
        if (!ok) {
            throw new AssertionError(what);
        }
    }

    static List<String> lines(String... lines) {
        return Arrays.asList(lines);
    }

    public static void main(String[] args) {
        Tests t = new Tests();
        PlayerCoreTest.register(t);
        CaptureTest.register(t);
        System.out.println(t.passed + " passed, " + t.failed.size() + " failed" + (t.failed.isEmpty() ? "" : ": " + t.failed));
        System.exit(t.failed.isEmpty() ? 0 : 1);
    }
}
