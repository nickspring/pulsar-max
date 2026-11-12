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
    final int channel;

    private Capture(int mask, int channel) {
        this.mask = mask;
        this.channels = Integer.bitCount(mask);
        this.channel = channel;
    }

    /** Null when the arguments are malformed or the channel is outside the mask. */
    static Capture parse(String maskArg, String channelArg) {
        int mask;
        int channel;
        try {
            mask = Integer.parseInt(maskArg, 16);
            channel = Integer.parseInt(channelArg);
        } catch (NumberFormatException e) {
            return null;
        }
        if (mask <= 0 || channel < 0 || channel >= Integer.bitCount(mask)) {
            return null;
        }
        return new Capture(mask, channel);
    }

    /** Writes the selected channel of {@code samples} interleaved samples as s16le; returns bytes. */
    int extract(short[] frames, int samples, byte[] mono) {
        int len = 0;
        for (int i = channel; i < samples; i += channels) {
            mono[len++] = (byte) frames[i];
            mono[len++] = (byte) (frames[i] >> 8);
        }
        return len;
    }

    /** Streams until a read error (exit code 1) or a closed output (exit code 0). */
    int pump(Source source, OutputStream out, Consumer<String> log) {
        short[] frames = new short[CHUNK_FRAMES * channels];
        byte[] mono = new byte[CHUNK_FRAMES * 2];
        while (true) {
            int n = source.read(frames);
            if (n < 0) {
                log.accept("AudioRecord: read error " + n);
                return 1;
            }
            int len = extract(frames, n, mono);
            try {
                out.write(mono, 0, len);
                out.flush();
            } catch (IOException e) {
                return 0;
            }
        }
    }
}
