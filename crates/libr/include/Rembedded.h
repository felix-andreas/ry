/*
 * ry's declaration of R's embedding entry points. ry's console is the `ry` binary; these start
 * and stop the runtime for programs that embed libR directly.
 */
#ifndef R_EMBEDDED_H_
#define R_EMBEDDED_H_

#include <R_ext/Boolean.h>

#ifdef __cplusplus
extern "C" {
#endif

int Rf_initEmbeddedR(int argc, char *argv[]);
void Rf_endEmbeddedR(int fatal);
int Rf_initialize_R(int argc, char *argv[]);
void setup_Rmainloop(void);
void run_Rmainloop(void);
void R_ReplDLLinit(void);
int R_ReplDLLdo1(void);
void R_setStartTime(void);
void R_RunExitFinalizers(void);
void CleanEd(void);
void Rf_KillAllDevices(void);
void R_CleanTempDir(void);
void R_SaveGlobalEnv(void);
void fpu_setup(Rboolean start);

#ifdef __cplusplus
}
#endif

#endif
