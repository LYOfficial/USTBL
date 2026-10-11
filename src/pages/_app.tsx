import i18n from "i18next";
import type { AppProps } from "next/app";
import { useRouter } from "next/router";
import { initReactI18next } from "react-i18next";
import LauncherProviders from "@/components/special/launcher-providers";
import { useWebviewBehavior } from "@/hooks/webview-behavior";
import PageLayout from "@/layouts/page-layout";
import { localeResources } from "@/locales";
import "@/styles/globals.css";

i18n.use(initReactI18next).init({
  resources: localeResources,
  fallbackLng: "en",
  lng: "zh-Hans",
  interpolation: { escapeValue: false },
});

export default function App({ Component, pageProps }: AppProps) {
  const { pathname } = useRouter();
  useWebviewBehavior();
  const page = <Component {...pageProps} />;
  const trayOnly = pathname === "/tray-popup";
  return (
    <LauncherProviders trayOnly={trayOnly}>
      {trayOnly ? page : <PageLayout pathname={pathname}>{page}</PageLayout>}
    </LauncherProviders>
  );
}
