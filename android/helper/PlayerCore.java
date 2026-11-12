import java.io.IOException;
import java.io.Writer;
import java.util.Arrays;
import java.util.concurrent.Callable;
import java.util.function.Consumer;

/**
 * The player protocol of docs/architecture.md, "Протокол помощников", free of android.* so it is
 * tested on the host. Single-threaded: commands and Listener calls must come from one thread.
 */
final class PlayerCore {
    interface Backend {
        /** Listener calls arrive later on the core's thread, never from inside prepare. */
        Playback prepare(String url, float gain, Listener listener) throws IOException;
    }

    interface Playback {
        void start();

        void pause();

        void setGain(float gain);

        void release();
    }

    interface Listener {
        void prepared();

        void completed();

        void error(String reason);
    }

    interface Volume {
        int max();

        int index();

        void set(int index);
    }

    private final Writer out;
    private final Consumer<String> log;
    private final Runnable exit;
    private final Channel music;
    private final Channel announce;
    private Volume volume;
    private int volumeMax;
    private int reportedVolume = -1;

    /** {@code exit} ends the process and does not return. */
    PlayerCore(Writer out, Consumer<String> log, Runnable exit, Backend music, Backend announce) {
        this.out = out;
        this.log = log;
        this.exit = exit;
        this.music = new Channel("music", music);
        this.announce = new Channel("announce", announce);
    }

    void start(Callable<Volume> openVolume) {
        try {
            Volume v = openVolume.call();
            int max = v.max();
            if (max > 0) {
                volume = v;
                volumeMax = max;
            } else {
                log.accept("volume disabled: max " + max);
            }
        } catch (Exception e) {
            disableVolume(e);
        }
        emit("ready");
        reportVolume();
    }

    /** False once volume is unavailable, so the caller can stop polling. */
    boolean pollVolume() {
        reportVolume();
        return volume != null;
    }

    void command(String line) {
        String[] f = line.split("\t", -1);
        String cmd = f[0];
        Channel ch = f.length > 1 ? channel(f[1]) : null;
        if (cmd.equals("play") && f.length >= 4 && !f[1].isEmpty() && channel(f[2]) != null) {
            channel(f[2]).play(f[1], Arrays.copyOfRange(f, 3, f.length));
        } else if (cmd.equals("pause") && f.length == 2 && ch != null) {
            ch.pause();
        } else if (cmd.equals("resume") && f.length == 2 && ch != null) {
            ch.resume();
        } else if (cmd.equals("stop") && f.length == 2 && ch != null) {
            ch.stop();
        } else if (cmd.equals("gain") && f.length == 3 && ch != null && isUnit(f[2])) {
            ch.setGain(Float.parseFloat(f[2]));
        } else if (cmd.equals("volume") && f.length == 2 && isUnit(f[1])) {
            setVolume(Float.parseFloat(f[1]), line);
        } else {
            log.accept("bad command: " + line);
        }
    }

    void shutdown() {
        music.stop();
        announce.stop();
        exit.run();
    }

    private Channel channel(String name) {
        return name.equals(music.name) ? music : name.equals(announce.name) ? announce : null;
    }

    private static boolean isUnit(String s) {
        try {
            float v = Float.parseFloat(s);
            return v >= 0f && v <= 1f;
        } catch (NumberFormatException e) {
            return false;
        }
    }

    private void setVolume(float level, String line) {
        if (volume == null) {
            log.accept("volume unavailable, ignored: " + line);
            return;
        }
        try {
            volume.set(Math.round(level * volumeMax));
        } catch (RuntimeException e) {
            disableVolume(e);
            return;
        }
        reportVolume();
    }

    private void reportVolume() {
        if (volume == null) {
            return;
        }
        int index;
        try {
            index = volume.index();
        } catch (RuntimeException e) {
            disableVolume(e);
            return;
        }
        if (index != reportedVolume) {
            reportedVolume = index;
            emit("volume\t" + index + "\t" + volumeMax);
        }
    }

    // Volume is optional: a failing IAudioService must not take playback down.
    private void disableVolume(Exception e) {
        volume = null;
        log.accept("volume disabled: " + e);
    }

    private void emit(String line) {
        try {
            out.write(line);
            out.write('\n');
            out.flush();
        } catch (IOException e) {
            music.release();
            announce.release();
            exit.run();
        }
    }

    private final class Channel {
        final String name;
        private final Backend backend;
        private float gain = 1f;
        private String state = "idle";
        private Track track;
        private boolean paused;
        private String id;
        private String[] urls;
        private int next;

        Channel(String name, Backend backend) {
            this.name = name;
            this.backend = backend;
        }

        void play(String playId, String[] playUrls) {
            if (id != null) {
                release();
                emit("done\t" + id + "\t0");
            }
            id = playId;
            urls = playUrls;
            next = 0;
            paused = false;
            playNext();
        }

        void pause() {
            if (track == null || paused) {
                return;
            }
            paused = true;
            if (track.isPrepared) {
                track.playback.pause();
            }
            setState("paused");
        }

        void resume() {
            if (track == null || !paused) {
                return;
            }
            paused = false;
            if (track.isPrepared) {
                track.playback.start();
                setState("playing");
            } else {
                setState("loading");
            }
        }

        void stop() {
            if (id != null) {
                finish(false);
            }
        }

        void setGain(float g) {
            gain = g;
            if (track != null) {
                track.playback.setGain(g);
            }
        }

        void release() {
            if (track != null) {
                track.playback.release();
                track = null;
            }
        }

        private void playNext() {
            release();
            if (next == urls.length) {
                finish(true);
                return;
            }
            Track t = new Track();
            try {
                t.playback = backend.prepare(urls[next++], gain, t);
            } catch (IOException | RuntimeException e) {
                log.accept(name + ": " + e);
                fail();
                return;
            }
            track = t;
            setState(paused ? "paused" : "loading");
        }

        private void fail() {
            setState("error");
            finish(false);
        }

        private void finish(boolean ok) {
            release();
            emit("done\t" + id + "\t" + (ok ? 1 : 0));
            id = null;
            urls = null;
            setState("idle");
        }

        private void setState(String s) {
            if (!s.equals(state)) {
                state = s;
                emit("state\t" + name + "\t" + s);
            }
        }

        private final class Track implements Listener {
            Playback playback;
            boolean isPrepared;

            @Override
            public void prepared() {
                if (track != this) {
                    return;
                }
                isPrepared = true;
                if (!paused) {
                    playback.start();
                    setState("playing");
                }
            }

            @Override
            public void completed() {
                if (track == this) {
                    playNext();
                }
            }

            @Override
            public void error(String reason) {
                if (track == this) {
                    log.accept(name + ": " + reason);
                    fail();
                }
            }
        }
    }
}
