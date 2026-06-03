import type { ModalProps } from "@chakra-ui/react";
import type React from "react";
import type { ExtensionInfo } from "./index";

// extension-declared host-rendered UI surfaces, excluding slot insertions.
interface ExtensionBaseDefinition<TProps = object> {
  Component: React.ComponentType<TProps>;
}

export interface ExtensionHomeWidgetDefinition extends ExtensionBaseDefinition {
  key?: string;
  title: string;
  description?: string;
  icon?: string;
  defaultWidth?: number;
  minWidth?: number;
  maxWidth?: number;
}

export interface ExtensionSettingsPageDefinition extends ExtensionBaseDefinition {}

export interface ExtensionPageDefinition extends ExtensionBaseDefinition {
  routePath: string;
  isStandAlone?: boolean;
}

export interface ExtensionModalDefinition
  extends
    Omit<ModalProps, "children" | "isOpen">,
    ExtensionBaseDefinition<{
      params?: any;
      close: () => void;
    }> {
  key: string;
  title: string;
  params?: any;
}

export interface ExtensionContributionRegistration {
  homeWidget?: ExtensionHomeWidgetDefinition;
  homeWidgets?: ExtensionHomeWidgetDefinition[];
  settingsPage?: ExtensionSettingsPageDefinition;
  page?: ExtensionPageDefinition;
  pages?: ExtensionPageDefinition[];
  customModal?: ExtensionModalDefinition;
  customModals?: ExtensionModalDefinition[];
}

// host-bound contribution (definition + extension metadata).
export interface ExtensionContributionBase {
  identifier: string;
  resetKey: string;
  extension: ExtensionInfo;
}

export interface ExtensionHomeWidgetContribution
  extends ExtensionHomeWidgetDefinition, ExtensionContributionBase {}

export interface ExtensionSettingsPageContribution
  extends ExtensionSettingsPageDefinition, ExtensionContributionBase {}

export interface ExtensionPageContribution
  extends ExtensionPageDefinition, ExtensionContributionBase {}

export interface ExtensionModalContribution
  extends ExtensionModalDefinition, ExtensionContributionBase {}
