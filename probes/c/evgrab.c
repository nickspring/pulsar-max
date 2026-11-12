// Exclusive evdev grab check: does io.sdk stop seeing the device while we hold EVIOCGRAB?
// usage: evgrab <device> <seconds>
#include <fcntl.h>
#include <linux/input.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: evgrab <device> <seconds>\n");
        return 2;
    }
    int fd = open(argv[1], O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        perror("open");
        return 1;
    }
    if (ioctl(fd, EVIOCGRAB, 1) != 0) {
        perror("EVIOCGRAB");
        return 1;
    }
    printf("grabbed %s\n", argv[1]);
    fflush(stdout);
    time_t end = time(NULL) + atoi(argv[2]);
    struct pollfd p = {.fd = fd, .events = POLLIN};
    while (time(NULL) < end) {
        if (poll(&p, 1, 500) <= 0) {
            continue;
        }
        struct input_event ev;
        if (read(fd, &ev, sizeof ev) != sizeof ev) {
            break;
        }
        if (ev.type != EV_SYN) {
            printf("%ld.%06ld type=%u code=%u value=%d\n", (long)ev.time.tv_sec,
                   (long)ev.time.tv_usec, ev.type, ev.code, ev.value);
            fflush(stdout);
        }
    }
    ioctl(fd, EVIOCGRAB, 0);
    close(fd);
    return 0;
}
