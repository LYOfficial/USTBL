import {
  Center,
  UseToastOptions,
  useToast as chakraUseToast,
  useColorModeValue,
} from "@chakra-ui/react";
import React, { ReactNode, createContext, useContext, useMemo } from "react";
import { BeatLoader } from "react-spinners";

interface ToastContextProviderProps {
  children: ReactNode;
}

type ToastContextType = ReturnType<typeof chakraUseToast>;

const ToastContext = createContext<ToastContextType | null>(null);

export const ToastContextProvider: React.FC<ToastContextProviderProps> = ({
  children,
}) => {
  const chakraToast = chakraUseToast();
  const toastVariant = useColorModeValue("left-accent", "solid");

  const customToast = useMemo<ToastContextType>(
    () =>
      Object.assign(
        (options: UseToastOptions = {}) =>
          chakraToast({
            position: "bottom-left",
            duration: options.status === "loading" ? null : 3000,
            icon:
              options.status === "loading" ? (
                <Center h="100%" mt={0.5}>
                  <BeatLoader size={4} />
                </Center>
              ) : null,
            variant: toastVariant,
            isClosable: true,
            containerStyle: {
              minWidth: "2xs",
              userSelect: "none",
            },
            ...options,
          }),
        {
          close: chakraToast.close,
          closeAll: chakraToast.closeAll,
          isActive: chakraToast.isActive,
          promise: chakraToast.promise,
          update: chakraToast.update,
        }
      ),
    [chakraToast, toastVariant]
  );

  return (
    <ToastContext.Provider value={customToast}>
      {children}
    </ToastContext.Provider>
  );
};

export const useToast = (): ToastContextType => {
  const context = useContext(ToastContext);
  if (!context)
    throw new Error("useToast must be used within a ToastContextProvider");
  return context;
};
