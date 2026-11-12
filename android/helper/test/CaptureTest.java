import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.List;

final class CaptureTest {
    static void register(Tests t) {
        t.add("capture arguments", CaptureTest::parsesArguments);
        t.add("bad capture arguments", CaptureTest::rejectsBadArguments);
        t.add("extracts one channel as s16le", CaptureTest::extractsOneChannel);
        t.add("extract stops at the samples read", CaptureTest::extractStopsAtSamplesRead);
        t.add("pump streams until a read error", CaptureTest::pumpStreamsUntilReadError);
        t.add("pump writes short reads as they come", CaptureTest::pumpWritesShortReads);
        t.add("pump ends cleanly on a closed output", CaptureTest::pumpEndsOnClosedOutput);
    }

    private static void parsesArguments() {
        Capture c = Capture.parse("ffff", "1");
        Tests.eq(Arrays.asList(0xffff, 16, 1), Arrays.asList(c.mask, c.channels, c.channel));
        c = Capture.parse("0a", "1");
        Tests.eq(Arrays.asList(0xa, 2, 1), Arrays.asList(c.mask, c.channels, c.channel));
        c = Capture.parse("6000", "0");
        Tests.eq(Arrays.asList(0x6000, 2, 0), Arrays.asList(c.mask, c.channels, c.channel));
    }

    private static void rejectsBadArguments() {
        String[][] bad = {{"ffff", "16"}, {"0a", "2"}, {"zz", "1"}, {"0", "0"}, {"-1", "0"}, {"80000000", "0"},
                {"ffff", "-1"}, {"ffff", "x"}, {"", "0"}, {"ffff", ""}};
        for (String[] args : bad) {
            Tests.check(Capture.parse(args[0], args[1]) == null, Arrays.toString(args));
        }
    }

    private static void extractsOneChannel() {
        Capture c = Capture.parse("7", "1");
        short[] frames = {10, 0x1234, 30, 11, -2, 31, 12, Short.MIN_VALUE, 32};
        byte[] mono = new byte[6];
        Tests.eq(6, c.extract(frames, frames.length, mono));
        Tests.eq(Arrays.toString(new byte[]{0x34, 0x12, (byte) 0xfe, (byte) 0xff, 0x00, (byte) 0x80}), Arrays.toString(mono));
    }

    private static void extractStopsAtSamplesRead() {
        Capture c = Capture.parse("3", "1");
        short[] frames = {1, 2, 3, 4, 5, 99};
        byte[] mono = new byte[6];
        Tests.eq(4, c.extract(frames, 5, mono));
        Tests.eq(Arrays.toString(new byte[]{2, 0, 4, 0, 0, 0}), Arrays.toString(mono));
        Tests.eq(0, c.extract(frames, 1, mono));
    }

    private static void pumpStreamsUntilReadError() {
        Capture c = Capture.parse("ffff", "1");
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        List<String> log = new ArrayList<>();
        int[] reads = {0};
        int code = c.pump(frames -> {
            if (reads[0] == 3) {
                return -3;
            }
            Tests.eq(Capture.CHUNK_FRAMES * 16, frames.length);
            for (int i = 0; i < frames.length; i++) {
                frames[i] = (short) (i % 16 == 1 ? reads[0] * 1000 + i / 16 : -1);
            }
            reads[0]++;
            return frames.length;
        }, out, log::add);
        Tests.eq(1, code);
        Tests.eq(Collections.singletonList("AudioRecord: read error -3"), log);
        byte[] got = out.toByteArray();
        Tests.eq(3 * Capture.CHUNK_FRAMES * 2, got.length);
        for (int chunk = 0; chunk < 3; chunk++) {
            for (int f = 0; f < Capture.CHUNK_FRAMES; f++) {
                int at = 2 * (chunk * Capture.CHUNK_FRAMES + f);
                Tests.eq(chunk * 1000 + f, (got[at] & 0xff) | got[at + 1] << 8);
            }
        }
    }

    private static void pumpWritesShortReads() {
        Capture c = Capture.parse("3", "0");
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        int[] reads = {0};
        int code = c.pump(frames -> {
            if (reads[0]++ == 1) {
                return -1;
            }
            frames[0] = 7;
            frames[1] = 8;
            frames[2] = 9;
            frames[3] = 10;
            return 4;
        }, out, line -> { });
        Tests.eq(1, code);
        Tests.eq(Arrays.toString(new byte[]{7, 0, 9, 0}), Arrays.toString(out.toByteArray()));
    }

    private static void pumpEndsOnClosedOutput() {
        Capture c = Capture.parse("ffff", "1");
        List<String> log = new ArrayList<>();
        int[] reads = {0};
        int[] writes = {0};
        OutputStream closed = new OutputStream() {
            @Override
            public void write(int b) throws IOException {
                throw new IOException("Broken pipe");
            }

            @Override
            public void write(byte[] b, int off, int len) throws IOException {
                if (++writes[0] == 2) {
                    throw new IOException("Broken pipe");
                }
            }
        };
        int code = c.pump(frames -> {
            reads[0]++;
            return frames.length;
        }, closed, log::add);
        Tests.eq(0, code);
        Tests.eq(2, reads[0]);
        Tests.eq(Collections.emptyList(), log);
    }
}
