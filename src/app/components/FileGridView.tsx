import { memo, forwardRef } from 'react';
import { VirtuosoGrid } from 'react-virtuoso';
import { useThemeStore } from '@/app/stores/themeStore';
import { FileItem } from '@/app/hooks/useR2Files';
import { FolderMetadata } from '@/app/stores/folderSizeStore';
import { StorageConfig } from '@/app/lib/r2cache';
import FileGridTile from '@/app/components/FileGridTile';

interface FileGridViewProps {
  items: FileItem[];
  onItemClick: (item: FileItem) => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onDownload?: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
  onFolderDownload?: (item: FileItem) => void;
  onFolderRename?: (item: FileItem) => void;
  storageConfig?: StorageConfig | null;
  folderSizes?: Record<string, FolderMetadata>;
  selectedKeys?: Set<string>;
  onToggleSelection?: (key: string) => void;
  showFullPath?: boolean;
  onFocus?: (item: FileItem) => void;
}

const GRID_COLS: Record<string, string> = {
  compact: 'repeat(auto-fill, minmax(140px, 1fr))',
  default: 'repeat(auto-fill, minmax(180px, 1fr))',
  cozy: 'repeat(auto-fill, minmax(220px, 1fr))',
};

const GRID_GAP: Record<string, string> = {
  compact: '10px',
  default: '14px',
  cozy: '18px',
};

// VirtuosoGrid requires a forwarded-ref List component
const GridList = forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement> & { density: string }
>(function GridList({ children, style, density, ...props }, ref) {
  return (
    <div
      ref={ref}
      {...props}
      className={`file-grid ${density !== 'default' ? density : ''}`}
      style={{
        ...style,
        gridTemplateColumns: GRID_COLS[density] ?? GRID_COLS.default,
        gap: GRID_GAP[density] ?? GRID_GAP.default,
      }}
    >
      {children}
    </div>
  );
});

// VirtuosoGrid item wrapper — plain div, no extra styles needed
const GridItem = forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(function GridItem(
  { children, ...props },
  ref
) {
  return (
    <div ref={ref} {...props}>
      {children}
    </div>
  );
});

export default memo(function FileGridView({
  items,
  onItemClick,
  onDelete,
  onRename,
  onDownload,
  onFolderDelete,
  onFolderDownload,
  onFolderRename,
  storageConfig,
  folderSizes,
  selectedKeys,
  onToggleSelection,
  showFullPath,
  onFocus,
}: FileGridViewProps) {
  const density = useThemeStore((s) => s.density);

  return (
    <VirtuosoGrid
      style={{ height: '100%' }}
      data={items}
      components={{
        // Pass density into the list via closure — VirtuosoGrid's List cannot
        // receive arbitrary props, so we capture density from the outer scope.
        List: forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(function List(
          { children, style, ...props },
          ref
        ) {
          return (
            <GridList ref={ref} style={style} density={density} {...props}>
              {children}
            </GridList>
          );
        }),
        Item: GridItem,
      }}
      itemContent={(_idx, item) => (
        <FileGridTile
          item={item}
          storageConfig={storageConfig}
          folderMetadata={item.isFolder ? folderSizes?.[item.key] : undefined}
          isSelected={selectedKeys?.has(item.key) ?? false}
          showFullPath={showFullPath}
          onItemClick={onItemClick}
          onToggleSelection={onToggleSelection ?? (() => undefined)}
          onDelete={onDelete}
          onRename={onRename}
          onDownload={onDownload}
          onFolderDelete={onFolderDelete}
          onFolderDownload={onFolderDownload}
          onFolderRename={onFolderRename}
          onFocus={onFocus}
        />
      )}
    />
  );
});
