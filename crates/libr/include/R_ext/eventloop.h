/*
 * ry's declaration of R's event loop on Unix-alikes, which packages such as later and httpuv
 * use to run handlers when a file descriptor has input. Define HAVE_SYS_SELECT_H to get the
 * declarations that need fd_set, as with GNU R.
 */
#ifndef R_EXT_EVENTLOOP_H
#define R_EXT_EVENTLOOP_H

#ifdef HAVE_SYS_SELECT_H
#include <sys/select.h>
#endif
#ifdef HAVE_SYS_TIME_H
#include <sys/time.h>
#endif
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

#define XActivity 1
#define StdinActivity 2

typedef void (*InputHandlerProc)(void *userData);

typedef struct _InputHandler {
    int activity;
    int fileDescriptor;
    InputHandlerProc handler;
    struct _InputHandler *next;
    int active;
    void *userData;
} InputHandler;

InputHandler *addInputHandler(InputHandler *handlers, int fd, InputHandlerProc handler,
                              int activity);
InputHandler *getInputHandler(InputHandler *handlers, int fd);
int removeInputHandler(InputHandler **handlers, InputHandler *it);
#if defined(HAVE_SYS_SELECT_H) || defined(FD_SETSIZE)
InputHandler *getSelectedHandler(InputHandler *handlers, fd_set *mask);
fd_set *R_checkActivity(int usec, int ignore_stdin);
void R_runHandlers(InputHandler *handlers, fd_set *mask);
int R_SelectEx(int n, fd_set *readfds, fd_set *writefds, fd_set *exceptfds,
               struct timeval *timeout, void (*intr)(void));
#endif

extern InputHandler *R_InputHandlers;
extern void (*R_PolledEvents)(void);
extern int R_wait_usec;

#ifdef __cplusplus
}
#endif

#endif
