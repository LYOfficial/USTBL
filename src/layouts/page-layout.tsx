import { ComponentType, ReactNode } from "react";
import DiscoverLayout from "@/layouts/discover-layout";
import InstanceDetailsLayout from "@/layouts/instance-details-layout";
import InstancesLayout from "@/layouts/instances-layout";
import MainLayout from "@/layouts/main-layout";
import MessagesLayout from "@/layouts/messages-layout";
import SettingsLayout from "@/layouts/settings-layout";

const routeLayouts: {
  prefix: string;
  layouts: ComponentType<{ children: ReactNode }>[];
}[] = [
  { prefix: "/discover", layouts: [DiscoverLayout] },
  { prefix: "/settings", layouts: [SettingsLayout] },
  { prefix: "/messages", layouts: [MessagesLayout] },
  {
    prefix: "/instances/details",
    layouts: [InstancesLayout, InstanceDetailsLayout],
  },
  { prefix: "/instances", layouts: [InstancesLayout] },
];

export default function PageLayout({
  pathname,
  children,
}: {
  pathname: string;
  children: ReactNode;
}) {
  const layouts =
    routeLayouts.find(({ prefix }) => pathname.startsWith(prefix))?.layouts ||
    [];
  return (
    <MainLayout>
      {layouts.reduceRight(
        (content, Layout) => (
          <Layout>{content}</Layout>
        ),
        children
      )}
    </MainLayout>
  );
}
