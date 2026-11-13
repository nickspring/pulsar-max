import java.io.IOException;
import java.io.OutputStream;
import java.util.function.Consumer;

/** The capture protocol of docs/architecture.md, "Протокол помощников", free of android.*. */
final class Capture {
    static final int RATE = 48000;
    static final int CHUNK_FRAMES = RATE / 50;

    interface Source {
        /** Interleaved samples read into frames, or a negative AudioRecord error code. */
        int read(short[] frames);
    }

    final int mask;
    final int channels;
    final int[] selected;

    private Capture(int mask, int[] selected) {
        this.mask = mask;
        this.channels = Integer.bitCount(mask);
        this.selected = selected;
    }

    /** Null when the arguments are malformed or a channel is outside the mask. */
    static Capture parse(String maskArg, String channelsArg) {
        int mask;
        String[] parts = channelsArg.split(",", -1);
        int[] selected = new int[parts.length];
        try {
            mask = Integer.parseInt(maskArg, 16);
            for (int i = 0; i < parts.length; i++) {
                selected[i] = Integer.parseInt(parts[i]);
            }
        } catch (NumberFormatException e) {
            return null;
        }
        if (mask <= 0) {
            return null;
        }
        for (int c : selected) {
            if (c < 0 || c >= Integer.bitCount(mask)) {
                return null;
            }
        }
        return new Capture(mask, selected);
    }

    /** Writes the selected channels of {@code samples} interleaved samples as s16le; returns bytes. */
    int extract(short[] frames, int samples, byte[] out) {
        int len = 0;
        for (int f = 0; f + channels <= samples; f += channels) {
            for (int c : selected) {
                short v = frames[f + c];
                out[len++] = (byte) v;
                out[len++] = (byte) (v >> 8);
            }
        }
        return len;
    }

    /** Streams until a read error (exit code 1) or a closed output (exit code 0). */
    int pump(Source source, OutputStream out, Consumer<String> log) {
        short[] frames = new short[CHUNK_FRAMES * channels];
        byte[] picked = new byte[CHUNK_FRAMES * selected.length * 2];
        while (true) {
            int n = source.read(frames);
            if (n < 0) {
                log.accept("AudioRecord: read error " + n);
                return 1;
            }
            int len = extract(frames, n, picked);
            try {
                out.write(picked, 0, len);
                out.flush();
            } catch (IOException e) {
                return 0;
            }
        }
    }
}
