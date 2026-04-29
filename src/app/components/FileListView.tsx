/**
 * FileListView — CSS-grid list with sticky headers, density-aware row heights,
 * inline actions, and react-virtuoso virtualization.
 *
 * Layout strategy: PER-ROW GRID
 * Virtuoso inserts internal spacer divs at the top/bottom of its scroller for
 * scroll positioning. If the scroller is a `display: grid` container, those
 * spacers become grid items and are allocated implicit auto-sized rows, which
 * pushes the visible items down by hundreds of pixels.
 *
 * Solution: keep the scroller as a plain block container; make each row an
 * independent `display: grid; grid-template-columns: GRID_COLS` element. The
 * header is its own matching grid above the scroller. Both grids use the same
 * GRID_COLS string and live inside the same width-constrained parent, so the
 * columns line up visually without needing subgrid.
 */
import { forwardRef } from 'react';
import { Virtuoso, Components } from 'react-virtuoso';
import { useThemeStore } from '@/app/stores/themeStore';
import { FileItem } from '@/app/hooks/useR2Files';
import FileContextMenu from '@/app/components/FileContextMenu';
import FileListRow, { FlCheckbox } from '@/app/components/FileListRow';

type SortOrder = 'asc' | 'desc' | null;

interface FolderMetadata {
  size: number | 'loading' | 'error';
  fileCount: number | null;
  totalFileCount: number | null;
  lastModified: string | null;
}

interface FileListViewProps {
  items: FileItem[];
  selectedKeys: Set<string>;
  metadata: Record<string, FolderMetadata>;
  nameSort: SortOrder;
  sizeSort: SortOrder;
  modifiedSort: SortOrder;
  showFullPath?: boolean;
  onItemClick: (item: FileItem) => void;
  onToggleSelection: (key: string) => void;
  onSelectAll: () => void;
  onClearSelection: () => void;
  onToggleNameSort: () => void;
  onToggleSizeSort: () => void;
  onToggleModifiedSort: () => void;
  onDelete: (item: FileItem) => void;
  onRename: (item: FileItem) => void;
  onDownload?: (item: FileItem) => void;
  onFolderDelete?: (item: FileItem) => void;
  onFolderDownload?: (item: FileItem) => void;
  onFolderRename?: (item: FileItem) => void;
  onMove?: (item: FileItem) => void;
  onFocus?: (item: FileItem) => void;
}

const GRID_COLS = '28px minmax(280px, 1fr) 110px 160px 70px';

/** Sort arrow indicator */
function SortArrow({ order }: { order: SortOrder }) {
  if (!order) return null;
  return <span className="sort-arrow">{order === 'asc' ? '↑' : '↓'}</span>;
}

/**
 * Virtuoso List wrapper — plain block container. Must NOT be a grid:
 * Virtuoso adds top/bottom scroll spacers as direct children, and a grid
 * parent allocates implicit auto-sized rows for them, pushing the visible
 * items down by hundreds of pixels.
 */
const VirtuosoList = forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(
  function VirtuosoList({ children, style, ...props }, ref) {
    return (
      <div ref={ref} {...props} style={style}>
        {children}
      </div>
    );
  }
);

/**
 * Virtuoso item wrapper — each row is its own `display: grid` with the same
 * GRID_COLS as the sticky header. Independent grids per row, but identical
 * column templates and matching parent width keep the columns visually
 * aligned without needing subgrid.
 */
const VirtuosoItem = forwardRef<HTMLDivElement, React.HTMLAttributes<HTMLDivElement>>(
  function VirtuosoItem({ children, style, ...props }, ref) {
    return (
      <div
        ref={ref}
        {...props}
        style={{
          ...style,
          display: 'grid',
          gridTemplateColumns: GRID_COLS,
        }}
      >
        {children}
      </div>
    );
  }
);

export default function FileListView({
  items,
  selectedKeys,
  metadata,
  nameSort,
  sizeSort,
  modifiedSort,
  showFullPath = false,
  onItemClick,
  onToggleSelection,
  onSelectAll,
  onClearSelection,
  onToggleNameSort,
  onToggleSizeSort,
  onToggleModifiedSort,
  onDelete,
  onRename,
  onDownload,
  onFolderDelete,
  onFolderDownload,
  onFolderRename,
  onMove,
  onFocus,
}: FileListViewProps) {
  const density = useThemeStore((s) => s.density);

  const allSelected = items.length > 0 && selectedKeys.size === items.length;
  const someSelected = selectedKeys.size > 0 && selectedKeys.size < items.length;

  const handleMasterCheckbox = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (allSelected || someSelected) {
      onClearSelection();
    } else {
      onSelectAll();
    }
  };

  const handleMove = (item: FileItem) => {
    if (onMove) onMove(item);
  };

  const virtuosoComponents: Components<FileItem> = {
    List: VirtuosoList,
    Item: VirtuosoItem,
  };

  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
      {/* Sticky header — separate grid with same column widths */}
      <div
        className="file-list"
        style={{
          gridTemplateColumns: GRID_COLS,
          flexShrink: 0,
        }}
      >
        <div className="col-h no-pad">
          <FlCheckbox
            checked={allSelected}
            indeterminate={someSelected}
            onClick={handleMasterCheckbox}
          />
        </div>
        <div className="col-h" onClick={onToggleNameSort}>
          Name <SortArrow order={nameSort} />
        </div>
        <div className="col-h" onClick={onToggleSizeSort}>
          Size <SortArrow order={sizeSort} />
        </div>
        <div className="col-h" onClick={onToggleModifiedSort}>
          Modified <SortArrow order={modifiedSort} />
        </div>
        <div className="col-h" style={{ justifyContent: 'flex-end' }}>
          Actions
        </div>
      </div>

      {/* Virtualized rows */}
      <Virtuoso
        style={{ flex: 1 }}
        data={items}
        components={virtuosoComponents}
        itemContent={(_idx, item) => {
          const folderMeta = item.isFolder ? metadata[item.key] : undefined;
          return (
            <FileContextMenu
              item={item}
              onDownload={onDownload}
              onRename={onRename}
              onDelete={onDelete}
              onFolderDownload={onFolderDownload}
              onFolderRename={onFolderRename}
              onFolderDelete={onFolderDelete}
            >
              <FileListRow
                item={item}
                isSelected={selectedKeys.has(item.key)}
                density={density}
                showFullPath={showFullPath}
                folderMeta={folderMeta}
                onItemClick={(i) => {
                  if (onFocus) onFocus(i);
                  onItemClick(i);
                }}
                onToggleSelection={onToggleSelection}
                onDownload={onDownload}
                onRename={onRename}
                onDelete={onDelete}
                onFolderDownload={onFolderDownload}
                onFolderRename={onFolderRename}
                onFolderDelete={onFolderDelete}
                onMove={handleMove}
              />
            </FileContextMenu>
          );
        }}
      />
    </div>
  );
}
