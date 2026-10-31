import { t } from "i18next";
import { InvokeResponse } from "@/models/response";
import { isDev } from "@/utils/env";
import { localizeFileError } from "@/utils/file-error";
import { logger } from "@/utils/logging";

export function responseHandler(serviceDomain: string): MethodDecorator {
  return function (
    target: any,
    propertyKey: string | symbol,
    descriptor: TypedPropertyDescriptor<any>
  ): TypedPropertyDescriptor<any> | void {
    const originalMethod = descriptor.value;

    descriptor.value = async function (
      ...args: any[]
    ): Promise<InvokeResponse<any>> {
      try {
        const data = await originalMethod.apply(this, args);
        const message = t(
          `Services.${serviceDomain}.${String(propertyKey)}.success`
        );
        return { status: "success", message, data };
      } catch (error: any) {
        const message = t(
          `Services.${serviceDomain}.${String(propertyKey)}.error.title`
        );

        const fileError = localizeFileError(t, String(error));
        const details = fileError
          ? fileError
          : String(error)
            ? t(
                `Services.${serviceDomain}.${String(propertyKey)}.error.description.${String(error)}`,
                {
                  defaultValue: String(error),
                }
              )
            : "";

        // Session expiration is reported once by the native session event.
        const sessionExpired =
          serviceDomain === "account" && String(error) === "EXPIRED";
        if (isDev && !sessionExpired) {
          const errorSet = {
            Key: String(error),
            "Service Domain": serviceDomain,
            "Property Key": String(propertyKey),
            "Localized Message": message,
            "Localized Details": details,
          };
          console.table(errorSet);
        }
        if (!sessionExpired) {
          logger.error(
            `Invoke ${String(propertyKey)} failed: ${message} - ${details}`
          );
        }

        return { status: "error", message, details, raw_error: error };
      }
    };

    return descriptor;
  };
}
