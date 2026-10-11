import { ChakraProvider } from "@chakra-ui/react";
import { ReactNode } from "react";
import GlobalEventHandler from "@/components/special/global-event-handler";
import { GuidedTourProvider } from "@/components/special/guided-tour-provider";
import SharedModalsProvider from "@/components/special/shared-modals-provider";
import { LauncherConfigContextProvider } from "@/contexts/config";
import { GlobalDataContextProvider } from "@/contexts/global-data";
import { RoutingHistoryContextProvider } from "@/contexts/routing-history";
import { TaskContextProvider } from "@/contexts/task";
import { ToastContextProvider } from "@/contexts/toast";
import chakraExtendTheme from "@/styles/chakra-theme";

export default function LauncherProviders({
  children,
  trayOnly = false,
}: {
  children: ReactNode;
  trayOnly?: boolean;
}) {
  return (
    <ChakraProvider theme={chakraExtendTheme}>
      {trayOnly ? (
        children
      ) : (
        <ToastContextProvider>
          <RoutingHistoryContextProvider>
            <LauncherConfigContextProvider>
              <GlobalDataContextProvider>
                <GuidedTourProvider>
                  <SharedModalsProvider>
                    <TaskContextProvider>
                      <GlobalEventHandler>{children}</GlobalEventHandler>
                    </TaskContextProvider>
                  </SharedModalsProvider>
                </GuidedTourProvider>
              </GlobalDataContextProvider>
            </LauncherConfigContextProvider>
          </RoutingHistoryContextProvider>
        </ToastContextProvider>
      )}
    </ChakraProvider>
  );
}
