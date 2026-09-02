/**
 * Runtime facts collected the moment the reporter opens "Report a problem".
 * Everything here is shown in the dialog's meta strip before the issue is
 * opened — the report never carries anything the reporter has not seen.
 */
export interface AppReportInfo {
  appVersion: string;
  /** e.g. "macos 15.6 (aarch64)" */
  os: string;
  /** The webview's user agent — WebKit / WebView2 build included. */
  webview: string;
  /** Storage provider of the active account, or 'none'. */
  provider: string;
  theme: string;
  /** e.g. "1400x900 @2x" */
  window: string;
  /** e.g. "2560x1440" */
  screen: string;
  locale: string;
  /** ISO-8601, UTC. */
  capturedAt: string;
}

/** What the dialog already knows without asking the runtime. */
export interface ReportContext {
  provider: string;
  theme: string;
}

const hasWindow = (): boolean => typeof window !== 'undefined';

/**
 * The synchronous baseline a report always carries. Also the answer when the
 * runtime collectors (Tauri IPC) are unavailable — a report with a blank OS
 * field still beats no report.
 */
export const fallbackReportInfo = (context: ReportContext): AppReportInfo => ({
  appVersion: '',
  os: '',
  webview: typeof navigator === 'undefined' ? '' : (navigator.userAgent ?? ''),
  provider: context.provider,
  theme: context.theme,
  window: hasWindow()
    ? `${window.innerWidth}x${window.innerHeight} @${window.devicePixelRatio}x`
    : '',
  screen: hasWindow() ? `${window.screen.width}x${window.screen.height}` : '',
  locale: typeof navigator === 'undefined' ? '' : (navigator.language ?? ''),
  capturedAt: new Date().toISOString(),
});

/**
 * Collect the runtime info for a report. Tauri APIs are imported lazily and
 * each failure degrades to an empty field — in plain-browser dev and in tests
 * there is no Tauri at all.
 */
export const buildReportInfo = async (context: ReportContext): Promise<AppReportInfo> => {
  let appVersion = '';
  let os = '';

  try {
    const { getVersion } = await import('@tauri-apps/api/app');
    appVersion = await getVersion();
  } catch {
    // No Tauri runtime.
  }

  try {
    const { platform, version, arch } = await import('@tauri-apps/plugin-os');
    os = `${platform()} ${version()} (${arch()})`;
  } catch {
    // No Tauri runtime.
  }

  return { ...fallbackReportInfo(context), appVersion, os };
};
