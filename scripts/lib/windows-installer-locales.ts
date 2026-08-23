// Installer-facing languages are intentionally distinct from the Desktop
// controller locale. WiX emits one MSI per locale (and puts that locale in the
// filename), while NSIS embeds all selected languages in one setup executable.

export const WINDOWS_WIX_INSTALLER_LOCALES = ["en-US", "zh-CN"] as const;
export type WindowsWixInstallerLocale = (typeof WINDOWS_WIX_INSTALLER_LOCALES)[number];

export const WINDOWS_NSIS_INSTALLER_LANGUAGES = ["English", "SimpChinese"] as const;

// Windows Installer stores ProductLanguage as an LCID, not a BCP-47 tag.
// Keep this mapping in reviewed source so CI proves that the locale suffix is
// not merely cosmetic.
export const WINDOWS_WIX_PRODUCT_LANGUAGE: Readonly<
  Record<WindowsWixInstallerLocale, number>
> = {
  "en-US": 1033,
  "zh-CN": 2052,
};

export function isWindowsWixInstallerLocale(value: string): value is WindowsWixInstallerLocale {
  return (WINDOWS_WIX_INSTALLER_LOCALES as readonly string[]).includes(value);
}

export function wixInstallerLocaleFromMsiName(name: string): WindowsWixInstallerLocale | null {
  for (const locale of WINDOWS_WIX_INSTALLER_LOCALES) {
    if (name.endsWith(`_${locale}.msi`)) return locale;
  }
  return null;
}
