/* LD_PRELOAD shim: lets the real bluetoothd run on a kernel without AF_BLUETOOTH.
 * - mgmt control channel (BTPROTO_HCI + HCI_CHANNEL_CONTROL) -> AF_UNIX SOCK_SEQPACKET
 *   connected to a userspace mgmt emulator at $FAKEBT_MGMT (default /tmp/fake-mgmt.sock)
 * - every other AF_BLUETOOTH socket -> an inert AF_UNIX socket (bind/listen/sockopts succeed)
 * Test rig only; never shipped. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#define AF_BT 31
#define BTPROTO_HCI 1
#define HCI_CHANNEL_CONTROL 3
#define MAXFD 4096
static unsigned char kind[MAXFD]; /* 0 none, 1 hci (undecided), 2 mgmt, 3 dummy */

struct sockaddr_hci_ { sa_family_t hci_family; unsigned short hci_dev; unsigned short hci_channel; };

static int (*r_socket)(int, int, int);
static int (*r_bind)(int, const struct sockaddr *, socklen_t);
static int (*r_listen)(int, int);
static int (*r_setsockopt)(int, int, int, const void *, socklen_t);
static int (*r_getsockopt)(int, int, int, void *, socklen_t *);
static int (*r_close)(int);
static void init(void) {
    if (r_socket) return;
    r_socket = dlsym(RTLD_NEXT, "socket"); r_bind = dlsym(RTLD_NEXT, "bind");
    r_listen = dlsym(RTLD_NEXT, "listen"); r_setsockopt = dlsym(RTLD_NEXT, "setsockopt");
    r_getsockopt = dlsym(RTLD_NEXT, "getsockopt"); r_close = dlsym(RTLD_NEXT, "close");
}
static int is_bt_level(int level) { return level == 274 || level == 0 || level == 6 || level == 17 || level == 18; }

int socket(int domain, int type, int protocol) {
    init();
    if (domain != AF_BT) return r_socket(domain, type, protocol);
    int flags = type & (SOCK_NONBLOCK | SOCK_CLOEXEC);
    int fd = r_socket(AF_UNIX, SOCK_SEQPACKET | flags, 0);
    if (fd >= 0 && fd < MAXFD) kind[fd] = (protocol == BTPROTO_HCI) ? 1 : 3;
    return fd;
}
int bind(int fd, const struct sockaddr *addr, socklen_t len) {
    init();
    if (fd < 0 || fd >= MAXFD || !kind[fd]) return r_bind(fd, addr, len);
    if (kind[fd] == 1 && len >= sizeof(struct sockaddr_hci_) &&
        ((const struct sockaddr_hci_ *)addr)->hci_channel == HCI_CHANNEL_CONTROL) {
        struct sockaddr_un un = { .sun_family = AF_UNIX };
        const char *p = getenv("FAKEBT_MGMT");
        strncpy(un.sun_path, p ? p : "/tmp/fake-mgmt.sock", sizeof(un.sun_path) - 1);
        int fl = fcntl(fd, F_GETFL);
        fcntl(fd, F_SETFL, fl & ~O_NONBLOCK);
        int rc = connect(fd, (struct sockaddr *)&un, sizeof(un));
        fcntl(fd, F_SETFL, fl);
        if (rc < 0) { errno = ENODEV; return -1; }
        kind[fd] = 2;
        return 0;
    }
    kind[fd] = 3;
    sa_family_t fam = AF_UNIX; /* autobind so that listen() works */
    r_bind(fd, (struct sockaddr *)&fam, sizeof(fam));
    return 0;
}
int listen(int fd, int backlog) {
    init();
    if (fd >= 0 && fd < MAXFD && kind[fd] == 3) { r_listen(fd, backlog); return 0; }
    return r_listen(fd, backlog);
}
int setsockopt(int fd, int level, int name, const void *val, socklen_t len) {
    init();
    if (fd >= 0 && fd < MAXFD && kind[fd] && is_bt_level(level)) return 0;
    return r_setsockopt(fd, level, name, val, len);
}
int getsockopt(int fd, int level, int name, void *val, socklen_t *len) {
    init();
    if (fd >= 0 && fd < MAXFD && kind[fd] && is_bt_level(level)) { if (val && len) memset(val, 0, *len); return 0; }
    return r_getsockopt(fd, level, name, val, len);
}
int close(int fd) {
    init();
    if (fd >= 0 && fd < MAXFD) kind[fd] = 0;
    return r_close(fd);
}
