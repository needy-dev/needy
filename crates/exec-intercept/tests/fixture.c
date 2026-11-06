#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static void *exec_thread(void *unused) {
    (void)unused;
    char *args[] = {"thread-command", "-s", NULL};
    execve("/usr/bin/uname", args, environ);
    _exit(71);
}

static long virtual_memory(void) {
    FILE *file = fopen("/proc/self/statm", "r");
    long pages = -1;
    if (!file) return -1;
    if (fscanf(file, "%ld", &pages) != 1) pages = -1;
    fclose(file);
    return pages;
}

int main(int argc, char **argv) {
    if (argc != 2) return 70;
    if (strcmp(argv[1], "execveat") == 0) {
        int fd = open("/usr/bin/uname", O_RDONLY | O_CLOEXEC);
        if (fd < 0) return 72;
        char *args[] = {"untrusted-argv0", "-s", NULL};
        syscall(SYS_execveat, fd, "", args, environ, AT_EMPTY_PATH);
        return 73;
    }
    if (strcmp(argv[1], "thread") == 0) {
        pthread_t thread;
        if (pthread_create(&thread, NULL, exec_thread, NULL) != 0) return 74;
        pthread_join(thread, NULL);
        return 75;
    }
    if (strcmp(argv[1], "bytes") == 0) {
        char *args[] = {"forged-identity", "space argument", "quote'\"", "\xff", NULL};
        char *env[] = {"PATH=/usr/bin:/bin", "BINARY=\xfe", NULL};
        execve("/usr/bin/uname", args, env);
        return 76;
    }
    if (strcmp(argv[1], "invalid") == 0) {
        char *args[] = {"invalid", NULL};
        syscall(SYS_execve, (void *)1, args, environ);
        return errno == EFAULT ? 0 : 77;
    }
    if (strcmp(argv[1], "signal-status") == 0 || strcmp(argv[1], "signal-user") == 0) {
        int expected = strcmp(argv[1], "signal-user") == 0 ? SIGUSR1 : SIGTERM;
        pid_t child = fork();
        if (child < 0) return 79;
        if (child == 0) {
            char *args[] = {"sh", "-c", expected == SIGUSR1 ? "kill -USR1 $$" : "kill -TERM $$", NULL};
            execve("/bin/sh", args, environ);
            _exit(80);
        }
        int status;
        if (waitpid(child, &status, 0) != child) return 81;
        return WIFSIGNALED(status) && WTERMSIG(status) == expected ? 0 : 82;
    }
    if (strcmp(argv[1], "vfork") == 0) {
        if (virtual_memory() < 0) return 83;
        long before = virtual_memory();
        for (int index = 0; index < 24; index++) {
            pid_t child = vfork();
            if (child < 0) return 84;
            if (child == 0) {
                char *args[] = {"uname", NULL};
                execve("/usr/bin/uname", args, environ);
                _exit(85);
            }
            int status;
            if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 86;
        }
        return virtual_memory() == before ? 0 : 87;
    }
    return 78;
}
