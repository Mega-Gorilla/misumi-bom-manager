import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  ArrowLeft,
  Plus,
  Copy,
  Trash2,
  ListOrdered,
  Undo2,
  Redo2,
  Columns3,
  MoveHorizontal,
  Save,
  Download,
  Search,
  DownloadCloud,
  RefreshCw,
  History,
  ChevronDown,
  Check,
  Pencil,
  ShoppingCart,
  FileSpreadsheet,
  Upload,
  Link2Off,
} from "lucide-react";

interface Props {
  title: string;
  status: string;
  quickFilter: string;
  onBack: () => void;
  onSave: () => void;
  onAddRow: () => void;
  onDupRows: () => void;
  onDelRows: () => void;
  onRenumber: () => void;
  onManageColumns: () => void;
  onHistory: () => void;
  historyOpen: boolean;
  onExport: () => void;
  onRename: (name: string) => void;
  onQuickFilter: (text: string) => void;
  onUndo: () => void;
  onRedo: () => void;
  onAutoSize: () => void;
  onQuote: (force: boolean) => void;
  quoting: boolean;
  onAddToCart: () => void;
  addingCart: boolean;
  /** Excel リンクモード (§4.8): 編集系を無効化し、Excel で編集/更新/反映/解除の導線に置き換える。 */
  link?: {
    onEditInExcel: () => void;
    onRefresh: () => void;
    onApply: () => void;
    onUnlink: () => void;
    refreshing: boolean;
    applying: boolean;
    /** §9-9: stale/missing 中は EC 取得・カート投入を止める。 */
    calcUsable: boolean;
  };
}

const ICON = 16;
const MENU_ICON = 15;

type MenuKey = "file" | "edit" | "view" | "data";

// One top-level menu (ファイル / 編集 / …). Classic menu-bar behavior: click to toggle, and
// while any menu is open, hovering a sibling switches to it. Closing (click-outside / Esc) is
// handled once by the parent for the whole bar.
function TopMenu(props: {
  label: string;
  menuKey: MenuKey;
  open: MenuKey | null;
  setOpen: (k: MenuKey | null) => void;
  children: ReactNode;
}) {
  const isOpen = props.open === props.menuKey;
  return (
    <div className="menu">
      <button
        className={`menu-btn ${isOpen ? "open" : ""}`}
        onClick={() => props.setOpen(isOpen ? null : props.menuKey)}
        onMouseEnter={() => {
          if (props.open) props.setOpen(props.menuKey);
        }}
      >
        {props.label}
        <ChevronDown size={13} />
      </button>
      {isOpen && (
        <div className="menu-pop" role="menu">
          {props.children}
        </div>
      )}
    </div>
  );
}

function MenuItem(props: {
  icon?: ReactNode;
  label: string;
  hint?: string;
  onClick: () => void;
  disabled?: boolean;
  checked?: boolean;
}) {
  return (
    <button
      className="menu-item"
      role="menuitem"
      onClick={props.onClick}
      disabled={props.disabled}
    >
      <span className="mi-icon">{props.checked ? <Check size={14} /> : props.icon}</span>
      <span className="mi-label">{props.label}</span>
      {props.hint && <span className="mi-hint">{props.hint}</span>}
    </button>
  );
}

export function Toolbar(p: Props) {
  const [openMenu, setOpenMenu] = useState<MenuKey | null>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const titleRef = useRef<HTMLInputElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  // Close the open menu on outside click / Escape (only while a menu is open).
  useEffect(() => {
    if (!openMenu) return;
    const onDown = (e: MouseEvent) => {
      if (barRef.current && !barRef.current.contains(e.target as Node)) setOpenMenu(null);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpenMenu(null);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [openMenu]);

  // Global shortcuts, kept in refs so the listener is registered once. Ctrl+S saves (the menu
  // advertises it), Ctrl+F focuses the quick filter (more useful than the webview's find).
  const saveRef = useRef(p.onSave);
  saveRef.current = p.onSave;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      const k = e.key.toLowerCase();
      if (k === "s") {
        e.preventDefault();
        saveRef.current();
      } else if (k === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Wrap a menu action so selecting it also closes the menu.
  const run = (fn: () => void) => () => {
    setOpenMenu(null);
    fn();
  };

  // §9-9: stale/missing の値 (型番・数量・小計…) を EC 取得・カート投入に使わない。
  const ecBlocked = !!p.link && !p.link.calcUsable;

  return (
    <div className="topbar" ref={barRef}>
      {/* ---- row 1: menu bar + document title (Windows-app style) ---- */}
      <div className="menubar-row">
        <button className="icon-btn" onClick={p.onBack} title="一覧へ戻る">
          <ArrowLeft size={ICON} />
        </button>

        <div className="menubar">
          <TopMenu label="ファイル" menuKey="file" open={openMenu} setOpen={setOpenMenu}>
            {!p.link && (
              <MenuItem
                icon={<Save size={MENU_ICON} />}
                label="保存"
                hint="Ctrl+S"
                onClick={run(p.onSave)}
              />
            )}
            {p.link && (
              <>
                <MenuItem
                  icon={<FileSpreadsheet size={MENU_ICON} />}
                  label="Excel で編集"
                  hint="起動のみ・ロックしない"
                  onClick={run(p.link.onEditInExcel)}
                />
                <MenuItem
                  icon={<RefreshCw size={MENU_ICON} />}
                  label="更新（Excel から再読込）"
                  disabled={p.link.refreshing}
                  onClick={run(p.link.onRefresh)}
                />
                <MenuItem
                  icon={<Upload size={MENU_ICON} />}
                  label="Excel へ反映"
                  hint="EC 取得値を書き込み"
                  disabled={p.link.applying}
                  onClick={run(p.link.onApply)}
                />
                <div className="menu-sep" />
                <MenuItem
                  icon={<Link2Off size={MENU_ICON} />}
                  label="リンクを解除"
                  hint="従来 BOM 化"
                  onClick={run(p.link.onUnlink)}
                />
              </>
            )}
            <MenuItem
              icon={<Pencil size={MENU_ICON} />}
              label="名前を変更"
              onClick={run(() => {
                titleRef.current?.focus();
                titleRef.current?.select();
              })}
            />
            {!p.link && (
              <MenuItem
                icon={<Download size={MENU_ICON} />}
                label="書き出し（Excel / CSV）"
                onClick={run(p.onExport)}
              />
            )}
            <div className="menu-sep" />
            <MenuItem
              icon={<ArrowLeft size={MENU_ICON} />}
              label="一覧へ戻る"
              onClick={run(p.onBack)}
            />
          </TopMenu>

          <TopMenu label="編集" menuKey="edit" open={openMenu} setOpen={setOpenMenu}>
            {/* §9-3: リンク BOM は編集を全無効化 (編集は Excel に集約・§4.8)。 */}
            <MenuItem
              icon={<Plus size={MENU_ICON} />}
              label="行追加"
              disabled={!!p.link}
              onClick={run(p.onAddRow)}
            />
            <MenuItem
              icon={<Copy size={MENU_ICON} />}
              label="複製"
              disabled={!!p.link}
              onClick={run(p.onDupRows)}
            />
            <MenuItem
              icon={<Trash2 size={MENU_ICON} />}
              label="行削除"
              disabled={!!p.link}
              onClick={run(p.onDelRows)}
            />
            <MenuItem
              icon={<ListOrdered size={MENU_ICON} />}
              label="No. を振り直し"
              disabled={!!p.link}
              onClick={run(p.onRenumber)}
            />
            <div className="menu-sep" />
            <MenuItem
              icon={<Undo2 size={MENU_ICON} />}
              label="元に戻す"
              hint="Ctrl+Z"
              disabled={!!p.link}
              onClick={run(p.onUndo)}
            />
            <MenuItem
              icon={<Redo2 size={MENU_ICON} />}
              label="やり直し"
              hint="Ctrl+Y"
              disabled={!!p.link}
              onClick={run(p.onRedo)}
            />
          </TopMenu>

          <TopMenu label="表示" menuKey="view" open={openMenu} setOpen={setOpenMenu}>
            <MenuItem
              icon={<Columns3 size={MENU_ICON} />}
              label="列管理"
              hint={p.link ? "リンク BOM は再マッピングで変更" : undefined}
              disabled={!!p.link}
              onClick={run(p.onManageColumns)}
            />
            <MenuItem
              icon={<MoveHorizontal size={MENU_ICON} />}
              label="列幅を内容に合わせる"
              onClick={run(p.onAutoSize)}
            />
            <div className="menu-sep" />
            <MenuItem
              icon={<History size={MENU_ICON} />}
              label="履歴パネル"
              checked={p.historyOpen}
              onClick={run(p.onHistory)}
            />
          </TopMenu>

          <TopMenu label="データ" menuKey="data" open={openMenu} setOpen={setOpenMenu}>
            <MenuItem
              icon={<DownloadCloud size={MENU_ICON} />}
              label="MISUMI 一括取得"
              hint={ecBlocked ? "未再計算のため不可" : "本日取得済みはキャッシュ"}
              disabled={p.quoting || ecBlocked}
              onClick={run(() => p.onQuote(false))}
            />
            <MenuItem
              icon={<RefreshCw size={MENU_ICON} />}
              label="最新化（強制再取得）"
              hint={ecBlocked ? "未再計算のため不可" : "キャッシュ無視"}
              disabled={p.quoting || ecBlocked}
              onClick={run(() => p.onQuote(true))}
            />
            <div className="menu-sep" />
            <MenuItem
              icon={<ShoppingCart size={MENU_ICON} />}
              label="カートに追加（MISUMI）"
              hint={ecBlocked ? "未再計算のため不可" : "要ログイン"}
              disabled={p.addingCart || ecBlocked}
              onClick={run(p.onAddToCart)}
            />
          </TopMenu>
        </div>

        {p.status && <span className="status-msg">{p.status}</span>}

        {/* Document title, editable in place (click or ファイル > 名前を変更).
            Pushed to the right edge where the row has free space. */}
        <input
          ref={titleRef}
          className="bom-title"
          value={p.title}
          onChange={(e) => p.onRename(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === "Escape") e.currentTarget.blur();
          }}
          placeholder="BOM 名"
          title="クリックで BOM 名を変更"
        />
      </div>

      {/* ---- row 2: command bar (high-frequency actions + search) ---- */}
      <div className="actionbar">
        {!p.link && (
          <>
            <button className="icon-btn" onClick={p.onUndo} title="元に戻す (Ctrl+Z)">
              <Undo2 size={ICON} />
            </button>
            <button className="icon-btn" onClick={p.onRedo} title="やり直し (Ctrl+Y)">
              <Redo2 size={ICON} />
            </button>

            <span className="sep" />
            <button onClick={p.onAddRow} title="末尾に行を追加">
              <Plus size={ICON} /> 行追加
            </button>
            <button className="icon-btn" onClick={p.onDupRows} title="選択行を複製">
              <Copy size={ICON} />
            </button>
            <button className="icon-btn" onClick={p.onDelRows} title="選択行を削除">
              <Trash2 size={ICON} />
            </button>

            <span className="sep" />
          </>
        )}
        {p.link && (
          <>
            <button onClick={p.link.onEditInExcel} title="Excel でファイルを開くだけ（ロックも受け渡しも無い・§9-6）">
              <FileSpreadsheet size={ICON} /> Excel で編集
            </button>
            <button
              onClick={p.link.onRefresh}
              disabled={p.link.refreshing}
              title="Excel から再読込 → 構造検証 → 再合成"
            >
              <RefreshCw size={ICON} /> 更新
            </button>
            <button
              onClick={p.link.onApply}
              disabled={p.link.applying}
              title="EC 取得値をアプリ所有列へ書き込み（Excel が開いていれば反映待ち）"
            >
              <Upload size={ICON} /> Excel へ反映
            </button>

            <span className="sep" />
          </>
        )}
        <button
          onClick={() => p.onQuote(false)}
          disabled={p.quoting || ecBlocked}
          title={
            ecBlocked
              ? "未再計算（stale/missing）の値は EC 取得に使えません。Excel で再計算・保存後に「更新」してください"
              : "ORDER=MISUMI の行を取得（本日取得済みはキャッシュを使用。日付が変わった型番・未取得のみ再取得）"
          }
        >
          <DownloadCloud size={ICON} /> MISUMI 一括取得
        </button>
        <button
          className="icon-btn"
          onClick={() => p.onQuote(true)}
          disabled={p.quoting || ecBlocked}
          title="最新化（キャッシュを無視して全件を再取得）"
        >
          <RefreshCw size={ICON} />
        </button>
        <button
          onClick={p.onAddToCart}
          disabled={p.addingCart || ecBlocked}
          title={
            ecBlocked
              ? "未再計算（stale/missing）の数量は使えません。Excel で再計算・保存後に「更新」してください"
              : "ORDER=MISUMI の行を MISUMI のカートにまとめて追加（要ログイン・注文ではなくカート投入）"
          }
        >
          <ShoppingCart size={ICON} /> カートに追加
        </button>
        <button
          className={p.historyOpen ? "active" : ""}
          onClick={p.onHistory}
          title="選択中の行の型番の履歴（価格・納期・EC在庫）を下部パネルに表示。開いている間は選択に追従"
        >
          <History size={ICON} /> 履歴
        </button>

        <span className="spacer" />
        <div className="search-box">
          <Search size={15} />
          <input
            ref={searchRef}
            className="quick-filter"
            value={p.quickFilter}
            onChange={(e) => p.onQuickFilter(e.currentTarget.value)}
            placeholder="検索 (Ctrl+F)"
          />
        </div>
      </div>
    </div>
  );
}
