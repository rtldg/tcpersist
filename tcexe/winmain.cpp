#define _CRT_SECURE_NO_WARNINGS
#include <Windows.h>
#include <TlHelp32.h>

#define LLDLL 0

#if LLDLL
#include <stdio.h>
#include "msgbox_bin.h" // generate from HxD->Export->C
#endif

#if NDEBUG
SERVICE_STATUS gSvcStatus = {};
SERVICE_STATUS_HANDLE gSvcStatusHandle = {};
#endif

HANDLE FindProcess(const wchar_t* exe)
{
	HANDLE ret = 0, hSnap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
	if (hSnap == INVALID_HANDLE_VALUE) return 0;

	PROCESSENTRY32W entry = { sizeof(entry) };

	if (Process32FirstW(hSnap, &entry)) {
		do {
			if (0 == _wcsicmp(entry.szExeFile, exe)) {
				ret = OpenProcess(PROCESS_ALL_ACCESS, FALSE, entry.th32ProcessID);
				break;
			}
		} while (Process32NextW(hSnap, &entry));
	}

	CloseHandle(hSnap);
	return ret;
}

void real_stuff()
{
	HANDLE token, process;
	OpenProcessToken((HANDLE)-1, TOKEN_ADJUST_PRIVILEGES, &token);
	TOKEN_PRIVILEGES tp = { 1, {0, SE_PRIVILEGE_ENABLED} };
	LookupPrivilegeValueW(NULL, SE_DEBUG_NAME, &tp.Privileges[0].Luid);
	AdjustTokenPrivileges(token, FALSE, &tp, sizeof(tp), NULL, NULL);

	while (!(process = FindProcess(L"calc.exe")))
		Sleep(1000);

	void* remotemem = VirtualAllocEx(process, 0, 256, MEM_COMMIT, PAGE_EXECUTE_READWRITE);
	if (remotemem == 0) return; // get rid of some dumb squiglys under calls

#if !LLDLL
#define shellcode \
		"\xBA\x00\x00\x00\x00"  /*  mov edx, 0x00000000 // MessageBoxA             */ \
		"\xB9\x00\x00\x00\x00"  /*  mov ecx, 0x00000000 // strings start           */ \
		"\x8D\x41\x0C"          /*  lea eax, [ecx+12]   // skip past title string  */ \
		"\x6A\x40"              /*  push 0x40 // type = MB_ICONINFORMATION         */ \
		"\x51"                  /*  push ecx  // caption/title                     */ \
		"\x50"                  /*  push eax  // text                              */ \
		"\x6A\x00"              /*  push 0    // hwnd                              */ \
		"\xFF\xD2"              /*  call edx                                       */ \
		"\xC3"                  /*  ret                                            */
	char buf[] =
		shellcode "\x00"
		"bottom text" "\x00"
		"imagine this is a cheat\nand not just shellcode\nfor a messagebox\n\nalso The Game";
	*(unsigned*)(buf + 1) = (unsigned)MessageBoxA;
	*(unsigned*)(buf + 6) = (unsigned)remotemem + sizeof(shellcode);

	WriteProcessMemory(process, remotemem, buf, sizeof(buf), NULL);
	CreateRemoteThread(process, NULL, 0, (LPTHREAD_START_ROUTINE)remotemem, 0, 0, NULL);
#else
	const char dllname[] = "C:\\Users\\Public\\msgbox.dll";
	FILE* f = fopen(dllname, "wb+");
	fwrite(msgbox_bin, 1, sizeof(msgbox_bin), f);
	fclose(f);

	WriteProcessMemory(process, remotemem, dllname, sizeof(dllname), NULL);
	CreateRemoteThread(process, NULL, 0, (LPTHREAD_START_ROUTINE)LoadLibraryA, remotemem, 0, NULL);
#endif
}

#if NDEBUG
void ReportSvcStatus(DWORD dwCurrentState, DWORD dwWin32ExitCode, DWORD dwWaitHint)
{
	static DWORD dwCheckPoint = 1;

	gSvcStatus.dwCurrentState = dwCurrentState;
	gSvcStatus.dwWin32ExitCode = dwWin32ExitCode;
	gSvcStatus.dwWaitHint = dwWaitHint;

	if (dwCurrentState == SERVICE_START_PENDING)
		gSvcStatus.dwControlsAccepted = 0;
	else
		gSvcStatus.dwControlsAccepted = SERVICE_ACCEPT_STOP;

	if ((dwCurrentState == SERVICE_RUNNING) || (dwCurrentState == SERVICE_STOPPED))
		gSvcStatus.dwCheckPoint = 0;
	else
		gSvcStatus.dwCheckPoint = dwCheckPoint++;

	SetServiceStatus(gSvcStatusHandle, &gSvcStatus);
}

void WINAPI SvcCtrlHandler(DWORD dwCtrl)
{
	if (dwCtrl == SERVICE_CONTROL_STOP) {
		ReportSvcStatus(SERVICE_STOPPED, NO_ERROR, 0);
		ExitProcess(0);
	}
}

VOID WINAPI SvcMain(DWORD dwArgc, LPWSTR* lpszArgv)
{
	gSvcStatusHandle = RegisterServiceCtrlHandlerW(L"", SvcCtrlHandler);
	gSvcStatus.dwServiceType = SERVICE_WIN32_OWN_PROCESS;
	gSvcStatus.dwServiceSpecificExitCode = 0;
	ReportSvcStatus(SERVICE_RUNNING, NO_ERROR, 0);
	real_stuff();
	ReportSvcStatus(SERVICE_STOPPED, NO_ERROR, 0);
}
#endif

int WINAPI wWinMain(
	_In_ HINSTANCE hInstance,
	_In_opt_ HINSTANCE hPrevInstance,
	_In_ LPWSTR lpCmdLine,
	_In_ int nShowCmd
)
{
#if NDEBUG
	SERVICE_TABLE_ENTRYW entries[2] = { {(LPWSTR)L"", SvcMain} };
	StartServiceCtrlDispatcherW(entries);
#else
	real_stuff();
#endif
	return 0;
}
