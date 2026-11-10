#include <dlfcn.h>
#include "my_application.h"

#define RUSTDESK_LIB_PATH "librustdesk.so"
// CanelaRemote (parche 34): el paquete con marca renombra el ejecutable (canelaremote) y su
// librería (lib/libcanelaremote.so). Los nombres salen del ejecutable; si esa librería no
// está junto a él, se usa la de upstream (el .deb genérico sigue igual).
#include <limits.h>
#include <unistd.h>
#include <string>
static std::string canela_exe_path() {
  char buf[PATH_MAX] = {0};
  ssize_t n = readlink("/proc/self/exe", buf, sizeof(buf) - 1);
  return n > 0 ? std::string(buf, n) : std::string();
}
std::string canela_exe_name() {
  std::string path = canela_exe_path();
  size_t slash = path.find_last_of('/');
  std::string name = slash == std::string::npos ? path : path.substr(slash + 1);
  return name.empty() ? std::string("rustdesk") : name;
}
static std::string canela_core_lib() {
  std::string path = canela_exe_path();
  size_t slash = path.find_last_of('/');
  if (slash != std::string::npos) {
    std::string lib = "lib" + canela_exe_name() + ".so";
    if (access((path.substr(0, slash) + "/lib/" + lib).c_str(), R_OK) == 0) return lib;
  }
  return RUSTDESK_LIB_PATH;
}
typedef bool (*RustDeskCoreMain)();
bool gIsConnectionManager = false;

void print_help_install_pkg(const char* so);

bool flutter_rustdesk_core_main() {
   const std::string core_lib = canela_core_lib();
   void* librustdesk = dlopen(core_lib.c_str(), RTLD_LAZY);
   if (!librustdesk) {
      fprintf(stderr,"Failed to load \"%s\"\n", core_lib.c_str());
      char* error;
      if ((error = dlerror()) != nullptr) {
        fprintf(stderr, "%s\n", error);
        char* libmissed = strstr(error, ": cannot open shared object file: No such file or directory");
        if (libmissed != nullptr) {
          *libmissed = '\0';
          char* so = strdup(error);
          print_help_install_pkg(so);
          free(so);
        }
      }
     return false;
   }
   auto core_main = (RustDeskCoreMain) dlsym(librustdesk,"rustdesk_core_main");
   char* error;
   if ((error = dlerror()) != nullptr) {
       fprintf(stderr, "Program entry \"rustdesk_core_main\" is not found: %s\n", error);
       return false;
   }
   return core_main();
}

int main(int argc, char** argv) {
  if (!flutter_rustdesk_core_main()) {
      return 0;
  }
  for (int i = 0; i < argc; i++) {
    if (strcmp(argv[i], "--cm") == 0) {
      gIsConnectionManager = true;
    }
  }
  g_autoptr(MyApplication) app = my_application_new();
  return g_application_run(G_APPLICATION(app), argc, argv);
}

typedef struct {
  const char* mgr;
  const char* search;
} PkgMgrSearch;

const PkgMgrSearch g_mgrs[] = {
  {
    "apt",
    "apt-file search",
  },
  {
    "dnf",
    "dnf provides",
  },
  {
    "yum",
    "yum provides",
  },
  {
    "zypper",
    "zypper wp",
  },
  {
    "pacman",
    "pacman -Qo",
  },
  {
    NULL,
    NULL,
  },
};

int is_command_exists(const char* command) {
    char* path = getenv("PATH");
    char* path_copy = strdup(path);
    char* dir = strtok(path_copy, ":");

    while (dir != NULL) {
        char command_path[256];
        snprintf(command_path, sizeof(command_path), "%s/%s", dir, command);
        if (access(command_path, X_OK) == 0) {
            free(path_copy);
            return 1;
        }
        dir = strtok(NULL, ":");
    }

    free(path_copy);
    return 0;
}

// We do not automatically search pkg 
// as the search process can be time consuming and update may be required.
void print_help_install_pkg(const char* so)
{
  if (strcmp(so, "libnsl.so.1") == 0) {
    const char* mgr[] = {"yum", "dnf", NULL};
    const char** m = mgr;
    while (*m != NULL) {
      if (is_command_exists(*m)) {
        fprintf(stderr, "Please run \"%s install libnsl\" to install the required package.\n", *m);
        return;
      }
      m++;
    }
  }

  const PkgMgrSearch *mgr_search = g_mgrs;
  while (mgr_search->mgr != NULL) {
      if (is_command_exists(mgr_search->mgr) == 1) {
        fprintf(stderr, "Please run \"%s %s\" to search and install the pkg.\n", mgr_search->search, so);
        break;
      }
      mgr_search++;
  }
}
