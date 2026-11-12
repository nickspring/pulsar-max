import android.app.Application;
import android.content.Context;
import android.content.ContextWrapper;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioRecord;
import android.media.MediaPlayer;
import android.media.MediaRecorder;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;

import java.io.BufferedOutputStream;
import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

/**
 * Audio helpers of pulsar-max, run as root through app_process on Android 9: Android wiring of
 * {@link PlayerCore} and {@link Capture}.
 */
public final class MaxHelper {
    private static final long VOLUME_POLL_MS = 500;
    private static final String SHELL_PACKAGE = "com.android.shell";

    @SuppressWarnings("deprecation")
    public static void main(String[] args) {
        try {
            Looper.prepareMainLooper();
            if (args.length == 1 && args[0].equals("player")) {
                player();
            } else if (args.length == 3 && args[0].equals("capture")) {
                fillActivityThread();
                System.exit(capture(args[1], args[2]));
            } else {
                System.exit(usage());
            }
        } catch (Exception e) {
            e.printStackTrace();
            System.exit(1);
        }
    }

    private static int usage() {
        System.err.println("usage: MaxHelper capture <hex channel index mask> <channel> | player");
        return 2;
    }

    // Android 9 AudioRecord JNI crashes on a null ActivityThread.currentOpPackageName().
    // Capture only: with a current application MediaPlayer starts a subtitle thread that needs
    // a real Context and kills the player.
    private static void fillActivityThread() throws ReflectiveOperationException {
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
            return SHELL_PACKAGE;
        }

        @Override
        public String getOpPackageName() {
            return SHELL_PACKAGE;
        }

        @Override
        public Context getApplicationContext() {
            return this;
        }
    }

    private static int capture(String maskArg, String channelArg) {
        Capture capture = Capture.parse(maskArg, channelArg);
        if (capture == null) {
            return usage();
        }
        int minMonoBytes = AudioRecord.getMinBufferSize(
                Capture.RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT);
        AudioRecord rec;
        try {
            rec = new AudioRecord.Builder()
                    .setAudioSource(MediaRecorder.AudioSource.MIC)
                    .setAudioFormat(new AudioFormat.Builder()
                            .setSampleRate(Capture.RATE)
                            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                            .setChannelIndexMask(capture.mask)
                            .build())
                    .setBufferSizeInBytes(Math.max(4 * capture.channels * minMonoBytes,
                            2 * capture.channels * Capture.RATE / 10))
                    .build();
        } catch (UnsupportedOperationException | IllegalArgumentException e) {
            System.err.println("AudioRecord: " + e.getMessage());
            return 1;
        }
        try {
            rec.startRecording();
            if (rec.getRecordingState() != AudioRecord.RECORDSTATE_RECORDING) {
                System.err.println("AudioRecord: startRecording failed");
                return 1;
            }
            return capture.pump(
                    frames -> rec.read(frames, 0, frames.length),
                    new BufferedOutputStream(new FileOutputStream(FileDescriptor.out), Capture.CHUNK_FRAMES * 2),
                    System.err::println);
        } finally {
            rec.release();
        }
    }

    private static void player() {
        Handler main = new Handler(Looper.getMainLooper());
        PlayerCore core = new PlayerCore(
                new BufferedWriter(new OutputStreamWriter(new FileOutputStream(FileDescriptor.out), StandardCharsets.UTF_8)),
                System.err::println,
                () -> System.exit(0),
                new MediaBackend(AudioAttributes.CONTENT_TYPE_MUSIC),
                new MediaBackend(AudioAttributes.CONTENT_TYPE_SPEECH));
        core.start(MusicVolume::new);
        main.postDelayed(new Runnable() {
            @Override
            public void run() {
                if (core.pollVolume()) {
                    main.postDelayed(this, VOLUME_POLL_MS);
                }
            }
        }, VOLUME_POLL_MS);
        new Thread(() -> readCommands(main, core), "stdin").start();
        Looper.loop();
    }

    private static void readCommands(Handler main, PlayerCore core) {
        try (BufferedReader in = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8))) {
            String line;
            while ((line = in.readLine()) != null) {
                String command = line;
                main.post(() -> core.command(command));
            }
        } catch (IOException e) {
            System.err.println("stdin: " + e);
        }
        main.post(core::shutdown);
    }

    private static final class MediaBackend implements PlayerCore.Backend {
        private final AudioAttributes attributes;

        MediaBackend(int contentType) {
            attributes = new AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(contentType)
                    .build();
        }

        @Override
        public PlayerCore.Playback prepare(String url, float gain, PlayerCore.Listener listener) throws IOException {
            MediaPlayer player = new MediaPlayer();
            try {
                player.setAudioAttributes(attributes);
                player.setOnPreparedListener(p -> listener.prepared());
                player.setOnCompletionListener(p -> listener.completed());
                player.setOnErrorListener((p, what, extra) -> {
                    listener.error("MediaPlayer error " + what + "/" + extra);
                    return true;
                });
                player.setDataSource(url);
                player.setVolume(gain, gain);
                player.prepareAsync();
            } catch (IOException | RuntimeException e) {
                player.release();
                throw e;
            }
            return new PlayerCore.Playback() {
                @Override
                public void start() {
                    player.start();
                }

                @Override
                public void pause() {
                    player.pause();
                }

                @Override
                public void setGain(float g) {
                    player.setVolume(g, g);
                }

                @Override
                public void release() {
                    player.release();
                }
            };
        }
    }

    // Reflection because IAudioService is hidden; signatures are those of Android 9.
    private static final class MusicVolume implements PlayerCore.Volume {
        private final Object service;
        private final Method getIndex;
        private final Method getMax;
        private final Method setIndex;

        MusicVolume() throws ReflectiveOperationException {
            IBinder binder = (IBinder) Class.forName("android.os.ServiceManager")
                    .getMethod("getService", String.class)
                    .invoke(null, Context.AUDIO_SERVICE);
            if (binder == null) {
                throw new IllegalStateException("audio service not found");
            }
            service = Class.forName("android.media.IAudioService$Stub")
                    .getMethod("asInterface", IBinder.class)
                    .invoke(null, binder);
            Class<?> iface = Class.forName("android.media.IAudioService");
            getIndex = iface.getMethod("getStreamVolume", int.class);
            getMax = iface.getMethod("getStreamMaxVolume", int.class);
            setIndex = iface.getMethod("setStreamVolume", int.class, int.class, int.class, String.class);
        }

        @Override
        public int max() {
            return (Integer) call(getMax, AudioManager.STREAM_MUSIC);
        }

        @Override
        public int index() {
            return (Integer) call(getIndex, AudioManager.STREAM_MUSIC);
        }

        @Override
        public void set(int index) {
            call(setIndex, AudioManager.STREAM_MUSIC, index, 0, SHELL_PACKAGE);
        }

        private Object call(Method method, Object... args) {
            try {
                return method.invoke(service, args);
            } catch (InvocationTargetException e) {
                throw new IllegalStateException("IAudioService." + method.getName() + ": " + e.getCause(), e.getCause());
            } catch (ReflectiveOperationException e) {
                throw new IllegalStateException("IAudioService." + method.getName() + ": " + e, e);
            }
        }
    }
}
