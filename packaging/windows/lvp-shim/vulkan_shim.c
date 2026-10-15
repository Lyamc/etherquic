/* Minimal "vulkan-1.dll" for hosts without a Vulkan loader.
 *
 * Granite (PyroWave) on Windows only does LoadLibraryA("vulkan-1.dll") and
 * GetProcAddress("vkGetInstanceProcAddr"). Mesa's lavapipe is an ICD and only
 * exports vk_icd* entry points, and the Khronos loader ignores VK_DRIVER_FILES
 * in elevated processes (our SYSTEM service). This shim loads vulkan_lvp.dll
 * from its own directory and forwards vkGetInstanceProcAddr to the ICD.
 * Nothing is registered system-wide. */
#include <windows.h>
#include <wchar.h>

typedef void (*PFN_void)(void);
typedef PFN_void (WINAPI *PFN_gipa)(void *instance, const char *name);
typedef int (WINAPI *PFN_negotiate)(unsigned *version);

static PFN_gipa real_gipa;
static INIT_ONCE once = INIT_ONCE_STATIC_INIT;

static BOOL CALLBACK load_icd(PINIT_ONCE o, PVOID p, PVOID *c)
{
	(void)o; (void)p; (void)c;
	wchar_t path[MAX_PATH + 32];
	HMODULE self = NULL;
	if (!GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
	                        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
	                        (LPCWSTR)(void *)&load_icd, &self))
		return TRUE;
	DWORD n = GetModuleFileNameW(self, path, MAX_PATH);
	if (n == 0 || n >= MAX_PATH)
		return TRUE;
	while (n > 0 && path[n - 1] != L'\\' && path[n - 1] != L'/')
		n--;
	path[n] = 0;
	wcscat(path, L"vulkan_lvp.dll");
	HMODULE icd = LoadLibraryExW(path, NULL, LOAD_WITH_ALTERED_SEARCH_PATH);
	if (!icd)
		return TRUE;
	PFN_negotiate negotiate =
		(PFN_negotiate)(void *)GetProcAddress(icd, "vk_icdNegotiateLoaderICDInterfaceVersion");
	if (negotiate) {
		unsigned version = 5;
		negotiate(&version);
	}
	real_gipa = (PFN_gipa)(void *)GetProcAddress(icd, "vk_icdGetInstanceProcAddr");
	return TRUE;
}

__declspec(dllexport) PFN_void WINAPI vkGetInstanceProcAddr(void *instance, const char *name)
{
	InitOnceExecuteOnce(&once, load_icd, NULL, NULL);
	return real_gipa ? real_gipa(instance, name) : NULL;
}
