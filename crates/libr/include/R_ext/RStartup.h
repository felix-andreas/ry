/*
 * ry's declaration of the start-up parameters an embedding front end chooses. ry's own front
 * end is the `ry` binary; the parameter block keeps GNU R's layout so front ends written for
 * GNU R compile and link unchanged.
 */
#ifndef R_EXT_RSTARTUP_H_
#define R_EXT_RSTARTUP_H_

#include <stddef.h>
#include <R_ext/Boolean.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    SA_NORESTORE,
    SA_RESTORE,
    SA_DEFAULT,
    SA_NOSAVE,
    SA_SAVE,
    SA_SAVEASK,
    SA_SUICIDE
} SA_TYPE;

#ifdef _WIN32
typedef enum { RGui, RTerm, LinkDLL } UImode;
#endif

/* The parameter block. Its layout is an ABI: front ends built against GNU R pass it by
 * pointer, so the fields, their order and the bit-field pair must not change. */
typedef struct {
    int R_Quiet;
    int R_NoEcho;
    int R_Interactive;
    int R_Verbose;
    int LoadSiteFile;
    int LoadInitFile;
    int DebugInitFile;
    SA_TYPE RestoreAction;
    SA_TYPE SaveAction;
    size_t vsize;
    size_t nsize;
    size_t max_vsize;
    size_t max_nsize;
    size_t ppsize;
    int NoRenviron : 16;
    int RstartVersion : 16;
    int nconnections;
#ifdef _WIN32
    char *rhome;
    char *home;
    int (*ReadConsole)(const char *, unsigned char *, int, int);
    void (*WriteConsole)(const char *, int);
    void (*CallBack)(void);
    void (*ShowMessage)(const char *);
    int (*YesNoCancel)(const char *);
    void (*Busy)(int);
    UImode CharacterMode;
    void (*WriteConsoleEx)(const char *, int, int);
    int EmitEmbeddedUTF8;
    void (*CleanUp)(SA_TYPE, int, int);
    void (*ClearerrConsole)(void);
    void (*FlushConsole)(void);
    void (*ResetConsole)(void);
    void (*Suicide)(const char *s);
#endif
} structRstart;

typedef structRstart *Rstart;

#define RSTART_VERSION 1

void R_DefParams(Rstart);
int R_DefParamsEx(Rstart, int);
void R_SetParams(Rstart);
void R_SizeFromEnv(Rstart);
void R_common_command_line(int *, char **, Rstart);
void R_set_command_line_arguments(int argc, char **argv);
void setup_Rmainloop(void);

#ifdef __cplusplus
}
#endif

#endif
