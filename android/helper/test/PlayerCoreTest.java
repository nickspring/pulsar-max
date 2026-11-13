import java.io.IOException;
import java.io.Writer;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Random;
import java.util.Set;

final class PlayerCoreTest {
    private static final List<String> NONE = Collections.emptyList();

    static void register(Tests t) {
        t.add("start emits ready then the volume", PlayerCoreTest::startEmitsReadyThenVolume);
        t.add("list plays in order then done ok", PlayerCoreTest::listPlaysInOrderThenDoneOk);
        t.add("replacing a playing play fails it once", PlayerCoreTest::replacingPlayingFailsItOnce);
        t.add("replacing while loading drops the old track", PlayerCoreTest::replacingWhileLoadingDropsOldTrack);
        t.add("replacing a paused play starts unpaused", PlayerCoreTest::replacingPausedStartsUnpaused);
        t.add("callbacks of released tracks are ignored", PlayerCoreTest::releasedTrackCallbacksAreIgnored);
        t.add("prepare error ends the play", PlayerCoreTest::prepareErrorEndsThePlay);
        t.add("playback error ends the play", PlayerCoreTest::playbackErrorEndsThePlay);
        t.add("setDataSource failure ends the play", PlayerCoreTest::dataSourceFailureEndsThePlay);
        t.add("setDataSource failure on a later url", PlayerCoreTest::dataSourceFailureOnLaterUrl);
        t.add("stop ends the play once", PlayerCoreTest::stopEndsThePlayOnce);
        t.add("stop while loading", PlayerCoreTest::stopWhileLoading);
        t.add("pause and resume while playing", PlayerCoreTest::pauseResumeWhilePlaying);
        t.add("pause while loading holds the start", PlayerCoreTest::pauseWhileLoadingHoldsStart);
        t.add("resume before prepared goes back to loading", PlayerCoreTest::resumeBeforePrepared);
        t.add("completion racing a pause keeps the next url paused", PlayerCoreTest::completionRacingPause);
        t.add("pause, resume, stop without a play are ignored", PlayerCoreTest::idleCommandsAreIgnored);
        t.add("gain persists per channel", PlayerCoreTest::gainPersistsPerChannel);
        t.add("malformed commands are logged and ignored", PlayerCoreTest::malformedCommands);
        t.add("volume command sets and always answers", PlayerCoreTest::volumeCommandSetsAndReports);
        t.add("volume poll reports knob changes", PlayerCoreTest::volumePollReportsKnob);
        t.add("volume open failure leaves playback working", PlayerCoreTest::volumeOpenFailure);
        t.add("volume with no max is disabled", PlayerCoreTest::volumeWithoutMax);
        t.add("volume max failure is disabled", PlayerCoreTest::volumeMaxFailure);
        t.add("volume index failure at start", PlayerCoreTest::volumeIndexFailureAtStart);
        t.add("volume poll failure is logged once", PlayerCoreTest::volumePollFailureLoggedOnce);
        t.add("volume set failure is logged once", PlayerCoreTest::volumeSetFailureLoggedOnce);
        t.add("shutdown ends both channels then exits", PlayerCoreTest::shutdownEndsBothChannels);
        t.add("shutdown when idle just exits", PlayerCoreTest::shutdownWhenIdle);
        t.add("closed stdout releases players and exits", PlayerCoreTest::closedStdoutReleasesAndExits);
        t.add("non-ascii id round trips", PlayerCoreTest::nonAsciiIdRoundTrips);
        t.add("random sequences: one done per play", PlayerCoreTest::randomSequencesOneDonePerPlay);
    }

    private static void startEmitsReadyThenVolume() {
        Fixture f = new Fixture();
        f.core.start(() -> f.volume);
        Tests.eq("ready\nvolume\t20\t100\n", f.out.raw());
        Tests.eq(NONE, f.log);
    }

    private static void listPlaysInOrderThenDoneOk() {
        Fixture f = Fixture.started();
        f.send("play\t1\tmusic\tu1\tu2");
        Tests.eq(Tests.lines("state\tmusic\tloading"), f.take());
        f.music.last().prepare();
        Tests.eq(Tests.lines("state\tmusic\tplaying"), f.take());
        f.music.last().complete();
        Tests.eq(Tests.lines("state\tmusic\tloading"), f.take());
        f.music.last().prepare();
        Tests.eq(Tests.lines("state\tmusic\tplaying"), f.take());
        f.music.last().complete();
        Tests.eq(Tests.lines("done\t1\t1", "state\tmusic\tidle"), f.take());
        Tests.eq(Tests.lines("music prepare u1 1.0", "music start u1", "music release u1",
                "music prepare u2 1.0", "music start u2", "music release u2"), f.takeCalls());
        Tests.eq(NONE, f.log);
    }

    private static void replacingPlayingFailsItOnce() {
        Fixture f = Fixture.started();
        f.send("play\t2\tannounce\ta1");
        f.announce.last().prepare();
        Tests.eq(Tests.lines("state\tannounce\tloading", "state\tannounce\tplaying"), f.take());
        f.takeCalls();
        f.send("play\t3\tannounce\ta2");
        Tests.eq(Tests.lines("done\t2\t0", "state\tannounce\tloading"), f.take());
        Tests.eq(Tests.lines("announce release a1", "announce prepare a2 1.0"), f.takeCalls());
        f.announce.last().prepare();
        f.announce.last().complete();
        Tests.eq(Tests.lines("state\tannounce\tplaying", "done\t3\t1", "state\tannounce\tidle"), f.take());
    }

    private static void replacingWhileLoadingDropsOldTrack() {
        Fixture f = Fixture.started();
        f.send("play\t2\tmusic\tm1");
        FakePlayback old = f.music.last();
        f.send("play\t3\tmusic\tm2");
        Tests.eq(Tests.lines("state\tmusic\tloading", "done\t2\t0"), f.take());
        Tests.eq(Tests.lines("music prepare m1 1.0", "music release m1", "music prepare m2 1.0"), f.takeCalls());
        old.prepare();
        old.complete();
        old.fail("late error");
        Tests.eq(NONE, f.take());
        Tests.eq(NONE, f.takeCalls());
        Tests.eq(NONE, f.log);
        f.music.last().prepare();
        f.music.last().complete();
        Tests.eq(Tests.lines("state\tmusic\tplaying", "done\t3\t1", "state\tmusic\tidle"), f.take());
    }

    private static void replacingPausedStartsUnpaused() {
        Fixture f = Fixture.started();
        f.send("play\t10\tmusic\tu1");
        f.music.last().prepare();
        f.send("pause\tmusic");
        Tests.eq(Tests.lines("state\tmusic\tloading", "state\tmusic\tplaying", "state\tmusic\tpaused"), f.take());
        f.send("play\t11\tmusic\tu2");
        Tests.eq(Tests.lines("done\t10\t0", "state\tmusic\tloading"), f.take());
        f.takeCalls();
        f.music.last().prepare();
        Tests.eq(Tests.lines("state\tmusic\tplaying"), f.take());
        Tests.eq(Tests.lines("music start u2"), f.takeCalls());
    }

    private static void releasedTrackCallbacksAreIgnored() {
        Fixture f = Fixture.started();
        f.send("play\t4\tmusic\tu1");
        FakePlayback old = f.music.last();
        old.prepare();
        f.send("stop\tmusic");
        Tests.eq(Tests.lines("state\tmusic\tloading", "state\tmusic\tplaying", "done\t4\t0", "state\tmusic\tidle"), f.take());
        f.takeCalls();
        old.complete();
        old.fail("late error");
        old.prepare();
        Tests.eq(NONE, f.take());
        Tests.eq(NONE, f.takeCalls());
        Tests.eq(NONE, f.log);
    }

    private static void prepareErrorEndsThePlay() {
        Fixture f = Fixture.started();
        f.send("play\t4\tmusic\tu1\tu2\tu3");
        f.music.last().prepare();
        f.music.last().complete();
        f.music.last().fail("MediaPlayer error 1/-1004");
        Tests.eq(Tests.lines("state\tmusic\tloading", "state\tmusic\tplaying", "state\tmusic\tloading",
                "state\tmusic\terror", "done\t4\t0", "state\tmusic\tidle"), f.take());
        Tests.eq(Tests.lines("music prepare u1 1.0", "music start u1", "music release u1",
                "music prepare u2 1.0", "music release u2"), f.takeCalls());
        Tests.eq(Tests.lines("music: MediaPlayer error 1/-1004"), f.log);
    }

    private static void playbackErrorEndsThePlay() {
        Fixture f = Fixture.started();
        f.send("play\t6\tannounce\ta1\ta2");
        f.announce.last().prepare();
        f.announce.last().fail("MediaPlayer error 100/0");
        Tests.eq(Tests.lines("state\tannounce\tloading", "state\tannounce\tplaying",
                "state\tannounce\terror", "done\t6\t0", "state\tannounce\tidle"), f.take());
        Tests.eq(1, f.announce.created.size());
        Tests.eq(Tests.lines("announce: MediaPlayer error 100/0"), f.log);
    }

    private static void dataSourceFailureEndsThePlay() {
        Fixture f = Fixture.started();
        f.send("play\t5\tmusic\tmissing\tu2");
        Tests.eq(Tests.lines("state\tmusic\terror", "done\t5\t0", "state\tmusic\tidle"), f.take());
        f.send("play\t6\tannounce\tbad");
        Tests.eq(Tests.lines("state\tannounce\terror", "done\t6\t0", "state\tannounce\tidle"), f.take());
        Tests.eq(Tests.lines("music prepare missing 1.0", "announce prepare bad 1.0"), f.takeCalls());
        Tests.eq(Tests.lines("music: java.io.IOException: setDataSource failed.",
                "announce: java.lang.IllegalArgumentException: bad url"), f.log);
        f.send("play\t7\tmusic\tu3");
        Tests.eq(Tests.lines("state\tmusic\tloading"), f.take());
    }

    private static void dataSourceFailureOnLaterUrl() {
        Fixture f = Fixture.started();
        f.send("play\t7\tmusic\tu1\tmissing");
        f.music.last().prepare();
        f.music.last().complete();
        Tests.eq(Tests.lines("state\tmusic\tloading", "state\tmusic\tplaying",
                "state\tmusic\terror", "done\t7\t0", "state\tmusic\tidle"), f.take());
        Tests.eq(0, f.music.live().size());
    }

    private static void stopEndsThePlayOnce() {
        Fixture f = Fixture.started();
        f.send("play\t7\tmusic\tu1");
        f.music.last().prepare();
        f.take();
        f.send("stop\tmusic");
        Tests.eq(Tests.lines("done\t7\t0", "state\tmusic\tidle"), f.take());
        f.send("stop\tmusic");
        Tests.eq(NONE, f.take());
        Tests.eq(0, f.music.live().size());
        Tests.eq(NONE, f.log);
    }

    private static void stopWhileLoading() {
        Fixture f = Fixture.started();
        f.send("play\t8\tannounce\ta1");
        f.send("stop\tannounce");
        Tests.eq(Tests.lines("state\tannounce\tloading", "done\t8\t0", "state\tannounce\tidle"), f.take());
        Tests.eq(Tests.lines("announce prepare a1 1.0", "announce release a1"), f.takeCalls());
    }

    private static void pauseResumeWhilePlaying() {
        Fixture f = Fixture.started();
        f.send("play\t8\tmusic\tu1");
        f.music.last().prepare();
        f.take();
        f.takeCalls();
        f.send("pause\tmusic");
        f.send("pause\tmusic");
        Tests.eq(Tests.lines("state\tmusic\tpaused"), f.take());
        f.send("resume\tmusic");
        f.send("resume\tmusic");
        Tests.eq(Tests.lines("state\tmusic\tplaying"), f.take());
        Tests.eq(Tests.lines("music pause u1", "music start u1"), f.takeCalls());
        f.music.last().complete();
        Tests.eq(Tests.lines("done\t8\t1", "state\tmusic\tidle"), f.take());
    }

    private static void pauseWhileLoadingHoldsStart() {
        Fixture f = Fixture.started();
        f.send("play\t9\tmusic\tu1");
        f.send("pause\tmusic");
        f.music.last().prepare();
        Tests.eq(Tests.lines("state\tmusic\tloading", "state\tmusic\tpaused"), f.take());
        Tests.eq(Tests.lines("music prepare u1 1.0"), f.takeCalls());
        f.send("resume\tmusic");
        Tests.eq(Tests.lines("state\tmusic\tplaying"), f.take());
        Tests.eq(Tests.lines("music start u1"), f.takeCalls());
    }

    private static void resumeBeforePrepared() {
        Fixture f = Fixture.started();
        f.send("play\t9\tmusic\tu1");
        f.send("pause\tmusic");
        f.send("resume\tmusic");
        Tests.eq(Tests.lines("state\tmusic\tloading", "state\tmusic\tpaused", "state\tmusic\tloading"), f.take());
        Tests.eq(Tests.lines("music prepare u1 1.0"), f.takeCalls());
        f.music.last().prepare();
        Tests.eq(Tests.lines("state\tmusic\tplaying"), f.take());
        Tests.eq(Tests.lines("music start u1"), f.takeCalls());
    }

    private static void completionRacingPause() {
        Fixture f = Fixture.started();
        f.send("play\t12\tmusic\tu1\tu2");
        f.music.last().prepare();
        f.send("pause\tmusic");
        f.music.last().complete();
        f.music.last().prepare();
        Tests.eq(Tests.lines("state\tmusic\tloading", "state\tmusic\tplaying", "state\tmusic\tpaused"), f.take());
        Tests.eq(Tests.lines("music prepare u1 1.0", "music start u1", "music pause u1", "music release u1",
                "music prepare u2 1.0"), f.takeCalls());
        f.send("resume\tmusic");
        Tests.eq(Tests.lines("state\tmusic\tplaying"), f.take());
        Tests.eq(Tests.lines("music start u2"), f.takeCalls());
    }

    private static void idleCommandsAreIgnored() {
        Fixture f = Fixture.started();
        for (String c : Tests.lines("pause\tmusic", "resume\tannounce", "stop\tmusic", "resume\tmusic", "stop\tannounce")) {
            f.send(c);
        }
        Tests.eq(NONE, f.take());
        Tests.eq(NONE, f.takeCalls());
        Tests.eq(NONE, f.log);
    }

    private static void gainPersistsPerChannel() {
        Fixture f = Fixture.started();
        f.send("gain\tmusic\t0.250");
        Tests.eq(NONE, f.takeCalls());
        f.send("play\t1\tmusic\tu1\tu2");
        f.music.last().prepare();
        f.send("gain\tmusic\t0.5");
        f.music.last().complete();
        f.send("play\t2\tannounce\ta1");
        f.send("gain\tannounce\t0");
        Tests.eq(Tests.lines("music prepare u1 0.25", "music start u1", "music gain u1 0.5", "music release u1",
                "music prepare u2 0.5", "announce prepare a1 1.0", "announce gain a1 0.0"), f.takeCalls());
        f.send("stop\tmusic");
        f.send("play\t3\tmusic\tu3");
        Tests.eq(Tests.lines("music release u2", "music prepare u3 0.5"), f.takeCalls());
        Tests.eq(NONE, f.log);
    }

    private static void malformedCommands() {
        Fixture f = Fixture.started();
        List<String> bad = Tests.lines("", "foo", "PLAY\t1\tmusic\tu", "play\t1\tradio\tu", "play\t1\tmusic",
                "play\t\tmusic\tu", "play", "pause", "pause\tradio", "resume\tmusic\t", "resume\tradio",
                "stop\tmusic\textra", "stop\tradio",
                "gain\tmusic", "gain\tradio\t0.5", "gain\tmusic\t2", "gain\tmusic\t-0.1", "gain\tmusic\tNaN",
                "gain\tmusic\tInfinity", "gain\tmusic\tx", "volume", "volume\tabc", "volume\t1.5",
                "volume\t0.5\t1");
        for (String b : bad) {
            f.send(b);
        }
        Tests.eq(NONE, f.take());
        Tests.eq(NONE, f.takeCalls());
        Tests.eq(NONE, f.volume.sets);
        List<String> want = new ArrayList<>();
        for (String b : bad) {
            want.add("bad command: " + b);
        }
        Tests.eq(want, f.log);
    }

    private static void volumeCommandSetsAndReports() {
        Fixture f = Fixture.started();
        f.send("volume\t0.500");
        Tests.eq(Tests.lines("volume\t50\t100"), f.take());
        f.send("volume\t0.5");
        Tests.eq(Tests.lines("volume\t50\t100"), f.take());
        Tests.check(f.core.pollVolume(), "poll keeps going");
        Tests.eq(NONE, f.take());
        f.send("volume\t0.333");
        f.send("volume\t0.336");
        f.send("volume\t1");
        f.send("volume\t0");
        Tests.eq(Tests.lines("volume\t33\t100", "volume\t34\t100", "volume\t100\t100", "volume\t0\t100"), f.take());
        Tests.eq(Tests.lines("50", "50", "33", "34", "100", "0"), f.volume.setsAsText());
        Tests.eq(NONE, f.log);
    }

    private static void volumePollReportsKnob() {
        Fixture f = Fixture.started();
        Tests.check(f.core.pollVolume(), "poll keeps going");
        Tests.eq(NONE, f.take());
        f.volume.index = 37;
        Tests.check(f.core.pollVolume(), "poll keeps going");
        Tests.eq(Tests.lines("volume\t37\t100"), f.take());
    }

    private static void volumeOpenFailure() {
        Fixture f = new Fixture();
        f.core.start(() -> {
            throw new ClassNotFoundException("android.media.IAudioService");
        });
        Tests.eq("ready\n", f.out.raw());
        f.out.take();
        Tests.eq(Tests.lines("volume disabled: java.lang.ClassNotFoundException: android.media.IAudioService"), f.log);
        assertRunsWithoutVolume(f);
    }

    private static void volumeWithoutMax() {
        Fixture f = new Fixture();
        f.volume.max = 0;
        f.core.start(() -> f.volume);
        Tests.eq("ready\n", f.out.raw());
        f.out.take();
        Tests.eq(Tests.lines("volume disabled: max 0"), f.log);
        assertRunsWithoutVolume(f);
    }

    private static void volumeMaxFailure() {
        Fixture f = new Fixture();
        f.volume.failMax = new IllegalStateException("IAudioService.getStreamMaxVolume: denied");
        f.core.start(() -> f.volume);
        Tests.eq("ready\n", f.out.raw());
        f.out.take();
        Tests.eq(Tests.lines("volume disabled: java.lang.IllegalStateException: IAudioService.getStreamMaxVolume: denied"), f.log);
        assertRunsWithoutVolume(f);
    }

    private static void volumeIndexFailureAtStart() {
        Fixture f = new Fixture();
        f.volume.failIndex = new IllegalStateException("IAudioService.getStreamVolume: dead");
        f.core.start(() -> f.volume);
        Tests.eq("ready\n", f.out.raw());
        f.out.take();
        Tests.eq(Tests.lines("volume disabled: java.lang.IllegalStateException: IAudioService.getStreamVolume: dead"), f.log);
        assertRunsWithoutVolume(f);
    }

    private static void volumePollFailureLoggedOnce() {
        Fixture f = Fixture.started();
        f.volume.failIndex = new IllegalStateException("IAudioService.getStreamVolume: dead");
        Tests.check(!f.core.pollVolume(), "poll stops");
        Tests.eq(NONE, f.take());
        Tests.eq(1, f.log.size());
        assertRunsWithoutVolume(f);
    }

    private static void volumeSetFailureLoggedOnce() {
        Fixture f = Fixture.started();
        f.volume.failSet = new IllegalStateException("IAudioService.setStreamVolume: java.lang.SecurityException: denied");
        f.send("volume\t0.1");
        Tests.eq(NONE, f.take());
        Tests.eq(Tests.lines("volume disabled: java.lang.IllegalStateException: IAudioService.setStreamVolume: "
                + "java.lang.SecurityException: denied"), f.log);
        assertRunsWithoutVolume(f);
    }

    private static void assertRunsWithoutVolume(Fixture f) {
        Tests.eq(1, f.log.size());
        Tests.check(f.log.get(0).startsWith("volume disabled: "), "volume failure logged: " + f.log);
        f.volume.failIndex = null;
        f.volume.failSet = null;
        f.volume.index = 55;
        Tests.check(!f.core.pollVolume(), "poll stops");
        Tests.check(!f.core.pollVolume(), "poll stays stopped");
        f.send("volume\t0.2");
        Tests.eq(NONE, f.take());
        Tests.eq(NONE, f.volume.sets);
        Tests.eq(2, f.log.size());
        Tests.eq("volume unavailable, ignored: volume\t0.2", f.log.get(1));
        f.send("play\t1\tmusic\tu1");
        f.music.last().prepare();
        f.music.last().complete();
        Tests.eq(Tests.lines("state\tmusic\tloading", "state\tmusic\tplaying", "done\t1\t1", "state\tmusic\tidle"), f.take());
    }

    private static void shutdownEndsBothChannels() {
        Fixture f = Fixture.started();
        f.send("play\t14\tmusic\tu1");
        f.music.last().prepare();
        f.send("play\t15\tannounce\ta1");
        f.take();
        expectExit(f.core::shutdown);
        Tests.eq(Tests.lines("done\t14\t0", "state\tmusic\tidle", "done\t15\t0", "state\tannounce\tidle"), f.take());
        Tests.eq(1, f.exits);
        Tests.eq(0, f.music.live().size() + f.announce.live().size());
    }

    private static void shutdownWhenIdle() {
        Fixture f = Fixture.started();
        expectExit(f.core::shutdown);
        Tests.eq(NONE, f.take());
        Tests.eq(1, f.exits);
    }

    private static void closedStdoutReleasesAndExits() {
        Fixture f = Fixture.started();
        f.send("play\t1\tmusic\tu1");
        f.music.last().prepare();
        f.send("play\t2\tannounce\ta1");
        f.take();
        f.takeCalls();
        f.out.closed = true;
        expectExit(() -> f.announce.last().prepare());
        Tests.eq(Tests.lines("announce start a1", "music release u1", "announce release a1"), f.takeCalls());
        Tests.eq(1, f.exits);
        Tests.eq(NONE, f.log);
    }

    private static void nonAsciiIdRoundTrips() {
        Fixture f = Fixture.started();
        f.send("play\tид-1\tmusic\thttp://h/трек.mp3");
        f.music.last().prepare();
        f.music.last().complete();
        Tests.eq("done\tид-1\t1", f.take().get(2));
        Tests.eq("music prepare http://h/трек.mp3 1.0", f.takeCalls().get(0));
    }

    /**
     * Random commands and callbacks against a fake that rejects MediaPlayer calls in a wrong state;
     * checks the protocol invariants after every step and after shutdown.
     */
    private static void randomSequencesOneDonePerPlay() {
        Random rnd = new Random(20261112);
        for (int run = 0; run < 500; run++) {
            Fuzz z = new Fuzz();
            for (int step = 0; step < 60; step++) {
                z.step(rnd);
            }
            expectExit(z.f.core::shutdown);
            z.consume();
            Tests.eq(z.urlCount.keySet(), z.done);
            Tests.eq(0, z.f.music.live().size() + z.f.announce.live().size());
        }
    }

    private static final class Fuzz {
        final Fixture f = Fixture.started();
        final Map<String, Integer> channelOf = new HashMap<>();
        final Map<String, Integer> urlCount = new HashMap<>();
        final Map<String, Integer> completions = new HashMap<>();
        final Set<String> done = new HashSet<>();
        final String[] current = new String[2];
        final String[] state = {"idle", "idle"};
        int nextId = 1;

        void step(Random rnd) {
            int c = rnd.nextInt(2);
            String name = c == 0 ? "music" : "announce";
            FakeBackend b = c == 0 ? f.music : f.announce;
            List<FakePlayback> live = b.live();
            FakePlayback p = live.isEmpty() ? null : live.get(0);
            switch (rnd.nextInt(10)) {
                case 0:
                case 1:
                    String id = String.valueOf(nextId++);
                    int n = 1 + rnd.nextInt(3);
                    StringBuilder line = new StringBuilder("play\t" + id + "\t" + name);
                    for (int i = 0; i < n; i++) {
                        line.append('\t').append(rnd.nextInt(8) == 0 ? "missing" : "u").append(i);
                    }
                    channelOf.put(id, c);
                    urlCount.put(id, n);
                    completions.put(id, 0);
                    current[c] = id;
                    f.send(line.toString());
                    break;
                case 2:
                    f.send("pause\t" + name);
                    break;
                case 3:
                    f.send("resume\t" + name);
                    break;
                case 4:
                    f.send("stop\t" + name);
                    break;
                case 5:
                    f.send("gain\t" + name + "\t" + rnd.nextInt(11) / 10.0);
                    break;
                case 6:
                    if (p != null && p.state == PState.PREPARING) {
                        p.prepare();
                    }
                    break;
                case 7:
                    if (p != null && p.state == PState.STARTED) {
                        completions.merge(current[c], 1, Integer::sum);
                        p.complete();
                    }
                    break;
                case 8:
                    if (p != null && rnd.nextInt(4) == 0) {
                        p.fail("MediaPlayer error 1/-1004");
                    }
                    break;
                default:
                    if (!b.created.isEmpty()) {
                        FakePlayback old = b.created.get(rnd.nextInt(b.created.size()));
                        if (old.state == PState.RELEASED) {
                            old.prepare();
                            old.complete();
                            old.fail("late");
                        }
                    }
                    break;
            }
            consume();
            checkChannel(0, f.music);
            checkChannel(1, f.announce);
        }

        void consume() {
            List<String> lines = f.take();
            for (int i = 0; i < lines.size(); i++) {
                String[] w = lines.get(i).split("\t", -1);
                Tests.eq(3, w.length);
                if (w[0].equals("done")) {
                    String id = w[1];
                    Tests.check(urlCount.containsKey(id), "done for an unknown id " + id);
                    Tests.check(done.add(id), "second done for " + id);
                    boolean ok = w[2].equals("1");
                    Tests.check(ok || w[2].equals("0"), "done flag " + w[2]);
                    Tests.eq(ok, completions.get(id).equals(urlCount.get(id)));
                    int c = channelOf.get(id);
                    if (id.equals(current[c])) {
                        current[c] = null;
                    }
                } else {
                    Tests.eq("state", w[0]);
                    int c = w[1].equals("music") ? 0 : 1;
                    Tests.check(!w[2].equals(state[c]), "repeated state " + lines.get(i));
                    state[c] = w[2];
                    if (w[2].equals("error")) {
                        Tests.check(i + 1 < lines.size() && lines.get(i + 1).startsWith("done\t")
                                && lines.get(i + 1).endsWith("\t0"), "error not followed by a failed done: " + lines);
                    }
                }
            }
            for (String id : urlCount.keySet()) {
                Tests.check(done.contains(id) || id.equals(current[channelOf.get(id)]), "replaced play without done: " + id);
            }
        }

        private void checkChannel(int c, FakeBackend b) {
            List<FakePlayback> live = b.live();
            Tests.check(live.size() <= 1, "more than one live player on " + b.channel);
            PState s = live.isEmpty() ? null : live.get(0).state;
            switch (state[c]) {
                case "idle":
                    Tests.check(s == null && current[c] == null, b.channel + " idle with a play");
                    break;
                case "loading":
                    Tests.eq(PState.PREPARING, s);
                    break;
                case "playing":
                    Tests.eq(PState.STARTED, s);
                    break;
                case "paused":
                    Tests.check(s == PState.PREPARING || s == PState.PREPARED || s == PState.PAUSED,
                            b.channel + " paused in " + s);
                    break;
                default:
                    throw new AssertionError(b.channel + " left in " + state[c]);
            }
        }
    }

    private static void expectExit(Runnable r) {
        try {
            r.run();
        } catch (Exit e) {
            return;
        }
        throw new AssertionError("no exit");
    }

    private static final class Exit extends RuntimeException {
        private static final long serialVersionUID = 1L;
    }

    private static final class Fixture {
        final Out out = new Out();
        final List<String> log = new ArrayList<>();
        final List<String> calls = new ArrayList<>();
        final FakeBackend music = new FakeBackend("music", calls);
        final FakeBackend announce = new FakeBackend("announce", calls);
        final FakeVolume volume = new FakeVolume();
        final PlayerCore core = new PlayerCore(out, log::add, this::exit, music, announce);
        int exits;

        static Fixture started() {
            Fixture f = new Fixture();
            f.core.start(() -> f.volume);
            Tests.eq(Tests.lines("ready", "volume\t20\t100"), f.take());
            return f;
        }

        void send(String line) {
            core.command(line);
        }

        List<String> take() {
            return out.take();
        }

        List<String> takeCalls() {
            List<String> c = new ArrayList<>(calls);
            calls.clear();
            return c;
        }

        private void exit() {
            exits++;
            throw new Exit();
        }
    }

    private static final class Out extends Writer {
        private final StringBuilder all = new StringBuilder();
        private int taken;
        boolean closed;

        @Override
        public void write(char[] buf, int off, int len) throws IOException {
            if (closed) {
                throw new IOException("Broken pipe");
            }
            all.append(buf, off, len);
        }

        @Override
        public void flush() throws IOException {
            if (closed) {
                throw new IOException("Broken pipe");
            }
        }

        @Override
        public void close() {
            closed = true;
        }

        String raw() {
            return all.toString();
        }

        List<String> take() {
            String s = all.substring(taken);
            taken = all.length();
            if (s.isEmpty()) {
                return NONE;
            }
            Tests.check(s.endsWith("\n") && !s.contains("\r"), "bad framing: " + s);
            List<String> lines = new ArrayList<>();
            Collections.addAll(lines, s.substring(0, s.length() - 1).split("\n", -1));
            return lines;
        }
    }

    private enum PState { PREPARING, PREPARED, STARTED, PAUSED, RELEASED }

    private static final class FakeBackend implements PlayerCore.Backend {
        final String channel;
        final List<String> calls;
        final List<FakePlayback> created = new ArrayList<>();

        FakeBackend(String channel, List<String> calls) {
            this.channel = channel;
            this.calls = calls;
        }

        @Override
        public PlayerCore.Playback prepare(String url, float gain, PlayerCore.Listener listener) throws IOException {
            calls.add(channel + " prepare " + url + " " + gain);
            if (url.startsWith("missing")) {
                throw new IOException("setDataSource failed.");
            }
            if (url.startsWith("bad")) {
                throw new IllegalArgumentException("bad url");
            }
            FakePlayback p = new FakePlayback(this, url, listener);
            created.add(p);
            return p;
        }

        FakePlayback last() {
            return created.get(created.size() - 1);
        }

        List<FakePlayback> live() {
            List<FakePlayback> live = new ArrayList<>();
            for (FakePlayback p : created) {
                if (p.state != PState.RELEASED) {
                    live.add(p);
                }
            }
            return live;
        }
    }

    /** Fails the test on calls MediaPlayer rejects in its current state, or on a double release. */
    private static final class FakePlayback implements PlayerCore.Playback {
        private final FakeBackend backend;
        private final String url;
        private final PlayerCore.Listener listener;
        PState state = PState.PREPARING;

        FakePlayback(FakeBackend backend, String url, PlayerCore.Listener listener) {
            this.backend = backend;
            this.url = url;
            this.listener = listener;
        }

        @Override
        public void start() {
            require(state == PState.PREPARED || state == PState.PAUSED, "start");
            state = PState.STARTED;
            record("start");
        }

        @Override
        public void pause() {
            require(state == PState.STARTED, "pause");
            state = PState.PAUSED;
            record("pause");
        }

        @Override
        public void setGain(float gain) {
            require(state != PState.RELEASED, "gain");
            backend.calls.add(backend.channel + " gain " + url + " " + gain);
        }

        @Override
        public void release() {
            require(state != PState.RELEASED, "release");
            state = PState.RELEASED;
            record("release");
        }

        void prepare() {
            if (state == PState.PREPARING) {
                state = PState.PREPARED;
            }
            listener.prepared();
        }

        void complete() {
            listener.completed();
        }

        void fail(String reason) {
            listener.error(reason);
        }

        private void record(String what) {
            backend.calls.add(backend.channel + " " + what + " " + url);
        }

        private void require(boolean ok, String op) {
            if (!ok) {
                throw new AssertionError(op + " in " + state + " for " + backend.channel + " " + url);
            }
        }
    }

    private static final class FakeVolume implements PlayerCore.Volume {
        final List<Integer> sets = new ArrayList<>();
        int max = 100;
        int index = 20;
        RuntimeException failMax;
        RuntimeException failIndex;
        RuntimeException failSet;

        @Override
        public int max() {
            if (failMax != null) {
                throw failMax;
            }
            return max;
        }

        @Override
        public int index() {
            if (failIndex != null) {
                throw failIndex;
            }
            return index;
        }

        @Override
        public void set(int i) {
            if (failSet != null) {
                throw failSet;
            }
            sets.add(i);
            index = i;
        }

        List<String> setsAsText() {
            List<String> s = new ArrayList<>();
            for (int i : sets) {
                s.add(String.valueOf(i));
            }
            return s;
        }
    }
}
