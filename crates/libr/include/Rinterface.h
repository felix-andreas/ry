/*
 * ry's declaration of R's front-end interface: the console functions, the hooks a front end
 * replaces, and the front-end data GNU R exports. Programs that embed libR and packages that
 * share the console (later, processx) use them.
 */
#ifndef R_INTERFACE_H_
#define R_INTERFACE_H_

#include <Rversion.h>

#ifdef __cplusplus
#include <cstdio>
#include <cstdint>
extern "C" {
#else
#include <stdio.h>
#include <stdint.h>
#endif

#include <R_ext/Boolean.h>
#include <R_ext/RStartup.h>

#if defined(__GNUC__) && __GNUC__ >= 3
#define R_INTERFACE_NORETURN __attribute__((noreturn))
#else
#define R_INTERFACE_NORETURN
#endif

extern Rboolean R_Interactive;
extern Rboolean R_NoEcho;

void R_RestoreGlobalEnv(void);
void R_RestoreGlobalEnvFromFile(const char *name, Rboolean quiet);
void R_SaveGlobalEnv(void);
void R_SaveGlobalEnvToFile(const char *name);
void R_FlushConsole(void);
void R_ClearerrConsole(void);
R_INTERFACE_NORETURN void R_Suicide(const char *message);
#if R_VERSION >= R_Version(4, 6, 0)
R_INTERFACE_NORETURN void R_CleanUp(SA_TYPE saveact, int status, int run_last);
#endif
char *R_HomeDir(void);
#if R_VERSION >= R_Version(4, 6, 0)
void R_Busy(int which);
int R_ReadConsole(const char *prompt, unsigned char *buf, int len, int addtohistory);
void R_WriteConsole(const char *buf, int len);
void R_WriteConsoleEx(const char *buf, int len, int otype);
void R_ResetConsole(void);
int R_EditFiles(int nfile, const char **file, const char **title, const char *editor);
#endif
void R_setupHistory(void);
void R_ProcessEvents(void);

extern int R_DirtyImage;
extern char *R_GUIType;
extern char *R_HistoryFile;
extern int R_HistorySize;
extern int R_RestoreHistory;
extern char *R_Home;
extern char *R_TempDir;
extern FILE *R_Consolefile;
extern FILE *R_Outputfile;
extern void *R_GlobalContext;
extern int R_running_as_main_program;
extern int R_SignalHandlers;
extern uintptr_t R_CStackLimit;
extern uintptr_t R_CStackStart;
extern int R_CStackDir;

#define jump_to_toplevel() Rf_error("")
#define mainloop run_Rmainloop
#define onintr Rf_onintr
#if R_VERSION >= R_Version(4, 6, 0)
void run_Rmainloop(void);
#endif
void Rf_onintr(void);
void Rf_jump_to_toplevel(void);
void process_site_Renviron(void);
void process_system_Renviron(void);
void process_user_Renviron(void);
void R_setStartTime(void);
void fpu_setup(Rboolean start);

#include <R_ext/Rdynload.h>

extern void (*ptr_R_Suicide)(const char *);
extern void (*ptr_R_ShowMessage)(const char *);
extern int (*ptr_R_ReadConsole)(const char *, unsigned char *, int, int);
extern void (*ptr_R_WriteConsole)(const char *, int);
extern void (*ptr_R_WriteConsoleEx)(const char *, int, int);
extern void (*ptr_R_ResetConsole)(void);
extern void (*ptr_R_FlushConsole)(void);
extern void (*ptr_R_ClearerrConsole)(void);
extern void (*ptr_R_Busy)(int);
extern void (*ptr_R_CleanUp)(SA_TYPE, int, int);
extern int (*ptr_R_ShowFiles)(int, const char **, const char **, const char *, Rboolean,
                              const char *);
extern int (*ptr_R_ChooseFile)(int, char *, int);
extern int (*ptr_R_EditFile)(const char *);
extern int (*ptr_R_EditFiles)(int, const char **, const char **, const char *);
extern void (*ptr_R_ProcessEvents)(void);
extern int (*R_timeout_handler)(void);
extern long R_timeout_val;
extern int R_isForkedChild;

#ifdef __cplusplus
}
#endif

#endif
