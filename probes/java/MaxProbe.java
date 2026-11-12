import android.app.Application;
import android.content.Context;
import android.content.ContextWrapper;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioRecord;
import android.media.AudioTrack;
import android.media.MediaRecorder;
import android.os.Looper;

import java.io.FileOutputStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;

/**
 * Audio probes for Yandex Station Max, run as root through app_process.
 *
 * record <seconds> <file> [hex channel index mask] [source] [tone amplitude 0..1]: s16 at 48 kHz;
 * a 440 Hz tone during seconds 2..4 shows the speaker reference channels and the VQE effect.
 */
public final class MaxProbe {
    private static final int RATE = 48000;

    public static void main(String[] args) throws Exception {
        Looper.prepareMainLooper();
        fillActivityThread();
        if (args.length >= 3 && args[0].equals("record")) {
            int seconds = Integer.parseInt(args[1]);
            String path = args[2];
            int mask = args.length > 3 ? Integer.parseInt(args[3], 16) : 0xffff;
            int source = args.length > 4 ? Integer.parseInt(args[4]) : MediaRecorder.AudioSource.MIC;
            float tone = args.length > 5 ? Float.parseFloat(args[5]) : 0f;
            if (tone < 0f || tone > 1f) {
                System.err.println("tone amplitude must be within 0..1");
                System.exit(2);
            }
            System.exit(record(seconds, path, mask, source, tone) ? 0 : 1);
        } else {
            System.err.println("usage: record <sec> <file> [mask] [source] [tone]");
            System.exit(2);
        }
    }

    // Android 9 AudioRecord takes opPackageName from ActivityThread.currentOpPackageName(), which
    // is null under app_process and crashes the JNI setup.
    private static void fillActivityThread() throws Exception {
        Class<?> atClass = Class.forName("android.app.ActivityThread");
        Constructor<?> ctor = atClass.getDeclaredConstructor();
        ctor.setAccessible(true);
        Object at = ctor.newInstance();
        Field current = atClass.getDeclaredField("sCurrentActivityThread");
        current.setAccessible(true);
        current.set(null, at);
        Application app = new Application();
        Field base = ContextWrapper.class.getDeclaredField("mBase");
        base.setAccessible(true);
        base.set(app, new ShellContext());
        Field initial = atClass.getDeclaredField("mInitialApplication");
        initial.setAccessible(true);
        initial.set(at, app);
    }

    private static final class ShellContext extends ContextWrapper {
        ShellContext() {
            super(null);
        }

        @Override
        public String getPackageName() {
            return "com.android.shell";
        }

        @Override
        public String getOpPackageName() {
            return "com.android.shell";
        }

        @Override
        public Context getApplicationContext() {
            return this;
        }
    }

    private static boolean record(int seconds, String path, int mask, int source, float tone) throws Exception {
        AudioFormat format = new AudioFormat.Builder()
                .setSampleRate(RATE)
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setChannelIndexMask(mask)
                .build();
        int channels = Integer.bitCount(mask);
        int frameBytes = 2 * channels;
        int minBuf = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT);
        int bufBytes = Math.max(minBuf * channels * 4, frameBytes * RATE / 10);
        AudioRecord rec;
        try {
            rec = new AudioRecord.Builder()
                    .setAudioSource(source)
                    .setAudioFormat(format)
                    .setBufferSizeInBytes(bufBytes)
                    .build();
        } catch (UnsupportedOperationException e) {
            System.out.println("AudioRecord failed: mask=0x" + Integer.toHexString(mask)
                    + " source=" + source + " buf=" + bufBytes + ": " + e.getMessage());
            return false;
        }
        System.out.println("channels=" + rec.getChannelCount()
                + " indexMask=0x" + Integer.toHexString(rec.getFormat().getChannelIndexMask())
                + " rate=" + rec.getSampleRate() + " session=" + rec.getAudioSessionId()
                + " buf=" + bufBytes);

        Thread player = null;
        if (tone > 0f) {
            player = new Thread(() -> playTone(tone, 2, 2), "tone");
        }

        byte[] buf = new byte[frameBytes * RATE / 50];
        long total = (long) frameBytes * RATE * seconds;
        long written = 0;
        try (FileOutputStream out = new FileOutputStream(path)) {
            rec.startRecording();
            System.out.println("recordingState=" + rec.getRecordingState());
            if (player != null) {
                player.start();
            }
            while (written < total) {
                int n = rec.read(buf, 0, (int) Math.min(buf.length, total - written));
                if (n < 0) {
                    System.out.println("read error " + n);
                    break;
                }
                out.write(buf, 0, n);
                written += n;
            }
            rec.stop();
        }
        rec.release();
        if (player != null) {
            player.join();
        }
        System.out.println("written=" + written + " bytes, frames=" + written / frameBytes);
        return written == total;
    }

    private static void playTone(float amplitude, int delaySec, int durationSec) {
        try {
            Thread.sleep(delaySec * 1000L);
            int frames = RATE * durationSec;
            short[] pcm = new short[frames * 2];
            for (int i = 0; i < frames; i++) {
                short s = (short) (amplitude * 32767 * Math.sin(2 * Math.PI * 440 * i / RATE));
                pcm[2 * i] = s;
                pcm[2 * i + 1] = s;
            }
            AudioTrack track = new AudioTrack.Builder()
                    .setAudioAttributes(new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                            .build())
                    .setAudioFormat(new AudioFormat.Builder()
                            .setSampleRate(RATE)
                            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                            .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                            .build())
                    .setTransferMode(AudioTrack.MODE_STATIC)
                    .setBufferSizeInBytes(pcm.length * 2)
                    .build();
            track.write(pcm, 0, pcm.length);
            track.play();
            Thread.sleep(durationSec * 1000L + 200);
            track.stop();
            track.release();
            System.out.println("tone done");
        } catch (Exception e) {
            System.out.println("tone failed: " + e);
        }
    }
}
