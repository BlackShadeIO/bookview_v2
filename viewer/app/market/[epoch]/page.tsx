import { MarketViewer } from '@/components/market-viewer/MarketViewer';

export default async function MarketPage({
  params,
}: {
  params: Promise<{ epoch: string }>;
}) {
  const { epoch } = await params;

  return <MarketViewer epoch={epoch} />;
}
