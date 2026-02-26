/* ═══════════════════════════════════════
   Mock Data — 统一管理
   ═══════════════════════════════════════ */

// ── Types ──

export type TaskStatus = "EXECUTING" | "WAITING" | "SCHEDULED" | "SUCCESS" | "ERROR";

export interface KeywordData {
    name: string;
    status: "ok" | "run" | "pending";
}

export interface CityData {
    name: string;
    progress: number;
    total: number;
    done: number;
    status: "active" | "done" | "pending";
    keywords: KeywordData[];
}

export interface MockTask {
    id: string;
    name: string;
    status: TaskStatus;
    assignedDevice?: string;   // 正在执行的设备 serial（空 = 未执行）
    cities: CityData[];
}

// ── 状态配色映射 ──

export const TASK_STATUS_STYLE: Record<TaskStatus, { color: string; bg: string; icon: string }> = {
    EXECUTING: { color: "var(--color-green)", bg: "rgba(16,185,129,.06)", icon: "●" },
    WAITING: { color: "var(--color-orange)", bg: "rgba(251,146,60,.04)", icon: "⏳" },
    SCHEDULED: { color: "var(--color-purple)", bg: "rgba(139,92,246,.04)", icon: "📋" },
    SUCCESS: { color: "var(--color-green)", bg: "rgba(16,185,129,.06)", icon: "✓" },
    ERROR: { color: "var(--color-red)", bg: "rgba(239,68,68,.04)", icon: "▲" },
};

// ── Mock 设备 ──

export interface MockDeviceInfo {
    serial: string;
    name: string;
    state: string;        // "Device" | "Offline"
    device_type: string;  // "usb" | "wifi"
}

export interface MockDeviceProps {
    serial: string;
    model: string;
    brand: string;
    android_version: string;
    sdk_version: string;
    display_resolution: string;
    device_type: string;
    battery_level: number;
    battery_temperature: number;
}

export const MOCK_DEVICES: MockDeviceInfo[] = [
    { serial: "R5CT90GXYWJ", name: "Galaxy S24 Ultra", state: "Device", device_type: "usb" },
    { serial: "PX6P-772A-09", name: "Pixel 6 Pro", state: "Device", device_type: "usb" },
    { serial: "HW-P50PRO-A3", name: "Huawei P50 Pro", state: "Device", device_type: "wifi" },
    { serial: "XMRN12-KK91", name: "Redmi Note 12", state: "Device", device_type: "usb" },
    { serial: "OP-NORD3-F8", name: "OnePlus Nord 3", state: "Device", device_type: "wifi" },
    { serial: "SM-G90F-22", name: "Samsung S22", state: "Offline", device_type: "usb" },
    { serial: "SONY-XP5V-01", name: "Xperia 5 V", state: "Offline", device_type: "wifi" },
    { serial: "VIVO-X90-PRO", name: "Vivo X90 Pro", state: "Offline", device_type: "usb" },
];

export const MOCK_DEVICE_PROPS: MockDeviceProps[] = [
    { serial: "R5CT90GXYWJ", model: "SM-S928B", brand: "Samsung", android_version: "14", sdk_version: "34", display_resolution: "1440x3120", device_type: "usb", battery_level: 92, battery_temperature: 31.5 },
    { serial: "PX6P-772A-09", model: "Pixel 6 Pro", brand: "Google", android_version: "14", sdk_version: "34", display_resolution: "1440x3120", device_type: "usb", battery_level: 98, battery_temperature: 46.2 },
    { serial: "HW-P50PRO-A3", model: "P50 Pro", brand: "Huawei", android_version: "12", sdk_version: "31", display_resolution: "1228x2700", device_type: "wifi", battery_level: 74, battery_temperature: 33.8 },
    { serial: "XMRN12-KK91", model: "Redmi Note 12", brand: "Xiaomi", android_version: "13", sdk_version: "33", display_resolution: "1080x2400", device_type: "usb", battery_level: 55, battery_temperature: 38.1 },
    { serial: "OP-NORD3-F8", model: "Nord 3", brand: "OnePlus", android_version: "13", sdk_version: "33", display_resolution: "1080x2412", device_type: "wifi", battery_level: 41, battery_temperature: 35.6 },
    { serial: "SM-G90F-22", model: "Galaxy S22", brand: "Samsung", android_version: "13", sdk_version: "33", display_resolution: "1080x2340", device_type: "usb", battery_level: 12, battery_temperature: 24.0 },
    { serial: "SONY-XP5V-01", model: "Xperia 5 V", brand: "Sony", android_version: "13", sdk_version: "33", display_resolution: "1080x2520", device_type: "wifi", battery_level: 8, battery_temperature: 22.3 },
    { serial: "VIVO-X90-PRO", model: "X90 Pro", brand: "Vivo", android_version: "13", sdk_version: "33", display_resolution: "1260x2800", device_type: "usb", battery_level: 3, battery_temperature: 21.1 },
];

// ── 全局任务池 ──

export const MOCK_TASKS: MockTask[] = [
    {
        id: "t1", name: "Global Lead Gen", status: "EXECUTING",
        assignedDevice: "R5CT90GXYWJ", // Galaxy S24 Ultra
        cities: [
            {
                name: "Tokyo", progress: 44, total: 32, done: 14, status: "active",
                keywords: [
                    { name: "Tech Startup", status: "ok" }, { name: "SaaS Company", status: "ok" }, { name: "Robotics Lab", status: "run" },
                    { name: "Design Agency", status: "pending" }, { name: "Coffee Shop", status: "ok" }, { name: "Co-working Space", status: "pending" },
                    { name: "Boutique Hotel", status: "pending" }, { name: "Public Library", status: "pending" }, { name: "Yoga Studio", status: "pending" },
                    { name: "Fitness Center", status: "pending" }, { name: "Art Gallery", status: "pending" }, { name: "Sushi Bar", status: "pending" },
                    { name: "Univ Campus", status: "pending" }, { name: "Museum", status: "pending" }, { name: "Cinema", status: "pending" },
                    { name: "Bakery", status: "pending" }, { name: "Law Firm", status: "pending" }, { name: "Dental Clinic", status: "pending" },
                ]
            },
            {
                name: "Seoul", progress: 100, total: 28, done: 28, status: "done",
                keywords: [
                    { name: "K-Beauty Store", status: "ok" }, { name: "PC Bang", status: "ok" }, { name: "Hanbok Rental", status: "ok" },
                    { name: "Fried Chicken", status: "ok" }, { name: "Language School", status: "ok" }, { name: "Cafe Chain", status: "ok" },
                    { name: "Co-living Space", status: "ok" }, { name: "Gaming Studio", status: "ok" }, { name: "Streetwear Shop", status: "ok" },
                ]
            },
            {
                name: "Osaka", progress: 0, total: 24, done: 0, status: "pending",
                keywords: [
                    { name: "Ramen Shop", status: "pending" }, { name: "Theme Park", status: "pending" }, { name: "Electronics Mall", status: "pending" },
                    { name: "Capsule Hotel", status: "pending" }, { name: "Vintage Store", status: "pending" }, { name: "Harbor Cruise", status: "pending" },
                ]
            },
        ]
    },
    {
        id: "t2", name: "Market Audit Pro", status: "EXECUTING",
        assignedDevice: "PX6P-772A-09", // Pixel 6 Pro
        cities: [
            {
                name: "New York", progress: 72, total: 40, done: 29, status: "active",
                keywords: [
                    { name: "Fintech Hub", status: "ok" }, { name: "Ad Agency", status: "ok" }, { name: "Media Company", status: "run" },
                    { name: "Law Practice", status: "ok" }, { name: "Fashion Brand", status: "run" }, { name: "Gallery Space", status: "pending" },
                    { name: "Coworking HQ", status: "ok" }, { name: "Restaurant Chain", status: "ok" }, { name: "Health Clinic", status: "pending" },
                ]
            },
            {
                name: "London", progress: 35, total: 36, done: 13, status: "active",
                keywords: [
                    { name: "Insurance Co", status: "ok" }, { name: "Pub & Bar", status: "run" }, { name: "Theatre Group", status: "pending" },
                    { name: "EdTech Firm", status: "ok" }, { name: "Royal Tours", status: "pending" }, { name: "Banking HQ", status: "ok" },
                ]
            },
            {
                name: "Berlin", progress: 0, total: 20, done: 0, status: "pending",
                keywords: [
                    { name: "Club Venue", status: "pending" }, { name: "Bike Rental", status: "pending" }, { name: "Startup Hub", status: "pending" },
                ]
            },
        ]
    },
    {
        id: "t3", name: "Data Scraper AI", status: "SCHEDULED",
        cities: [
            {
                name: "Shanghai", progress: 88, total: 50, done: 44, status: "active",
                keywords: [
                    { name: "Electronics", status: "ok" }, { name: "Fashion Mall", status: "ok" }, { name: "Tea House", status: "ok" },
                    { name: "Logistics Co", status: "run" }, { name: "B2B Platform", status: "ok" }, { name: "Street Market", status: "ok" },
                    { name: "IoT Factory", status: "run" }, { name: "Jewelry Store", status: "pending" }, { name: "Auto Parts", status: "ok" },
                ]
            },
            {
                name: "Singapore", progress: 60, total: 30, done: 18, status: "active",
                keywords: [
                    { name: "Hawker Center", status: "ok" }, { name: "Tech Park", status: "ok" }, { name: "Shipping Co", status: "run" },
                    { name: "Hotel Chain", status: "ok" }, { name: "MedTech Lab", status: "pending" }, { name: "Banking HQ", status: "ok" },
                ]
            },
        ]
    },
    {
        id: "t4", name: "Daily Sync Root", status: "SUCCESS",
        cities: [
            {
                name: "Local", progress: 100, total: 10, done: 10, status: "done",
                keywords: [
                    { name: "Config Sync", status: "ok" }, { name: "DB Backup", status: "ok" }, { name: "Log Rotate", status: "ok" },
                ]
            },
        ]
    },
    {
        id: "t5", name: "Social Monitor", status: "ERROR",
        cities: [
            {
                name: "Global", progress: 25, total: 20, done: 5, status: "active",
                keywords: [
                    { name: "Twitter Feed", status: "ok" }, { name: "Instagram API", status: "ok" }, { name: "TikTok Scrape", status: "pending" },
                    { name: "Reddit Watch", status: "pending" }, { name: "YouTube Alert", status: "pending" },
                ]
            },
        ]
    },
    {
        id: "t6", name: "Lead Enrichment", status: "SCHEDULED",
        cities: [
            {
                name: "Remote", progress: 0, total: 60, done: 0, status: "pending",
                keywords: [
                    { name: "Email Verify", status: "pending" }, { name: "Phone Lookup", status: "pending" }, { name: "Company Match", status: "pending" },
                ]
            },
        ]
    },
    {
        id: "t7", name: "SEO Backlink Audit", status: "WAITING",
        cities: [
            {
                name: "San Francisco", progress: 15, total: 45, done: 7, status: "active",
                keywords: [
                    { name: "Domain Authority", status: "ok" }, { name: "Anchor Text", status: "ok" }, { name: "Broken Links", status: "run" },
                    { name: "Redirect Chain", status: "pending" }, { name: "NoFollow Ratio", status: "pending" }, { name: "Spam Score", status: "pending" },
                    { name: "Link Velocity", status: "pending" }, { name: "Referring IPs", status: "pending" },
                ]
            },
            {
                name: "Toronto", progress: 0, total: 30, done: 0, status: "pending",
                keywords: [
                    { name: "Guest Posts", status: "pending" }, { name: "Forum Links", status: "pending" }, { name: "Directory Submit", status: "pending" },
                    { name: "Social Signals", status: "pending" }, { name: "Edu Links", status: "pending" },
                ]
            },
        ]
    },
    {
        id: "t8", name: "Proxy Validator", status: "SCHEDULED",
        cities: [
            {
                name: "Amsterdam", progress: 0, total: 120, done: 0, status: "pending",
                keywords: [
                    { name: "SOCKS5 Check", status: "pending" }, { name: "HTTP Tunnel", status: "pending" }, { name: "Latency Test", status: "pending" },
                    { name: "Geo Verify", status: "pending" }, { name: "Anonymity Level", status: "pending" }, { name: "Uptime Monitor", status: "pending" },
                ]
            },
        ]
    },
    {
        id: "t9", name: "Image Optimizer", status: "SUCCESS",
        cities: [
            {
                name: "CDN Global", progress: 100, total: 450, done: 450, status: "done",
                keywords: [
                    { name: "WebP Convert", status: "ok" }, { name: "AVIF Encode", status: "ok" }, { name: "Lazy Load", status: "ok" },
                    { name: "Resize 2x", status: "ok" }, { name: "Metadata Strip", status: "ok" }, { name: "Quality Tune", status: "ok" },
                ]
            },
        ]
    },
    {
        id: "t10", name: "Cloud Backup", status: "ERROR",
        cities: [
            {
                name: "AWS US-East", progress: 68, total: 80, done: 54, status: "active",
                keywords: [
                    { name: "DB Snapshot", status: "ok" }, { name: "File Archive", status: "ok" }, { name: "Config Export", status: "ok" },
                    { name: "Media Sync", status: "run" }, { name: "Log Bundle", status: "pending" }, { name: "Certificate Bak", status: "pending" },
                ]
            },
            {
                name: "GCP Asia", progress: 0, total: 40, done: 0, status: "pending",
                keywords: [
                    { name: "Bucket Mirror", status: "pending" }, { name: "KMS Rotate", status: "pending" }, { name: "IAM Audit", status: "pending" },
                ]
            },
        ]
    },
    {
        id: "t11", name: "Web Crawler v2", status: "WAITING",
        cities: [
            {
                name: "Bangkok", progress: 52, total: 65, done: 34, status: "active",
                keywords: [
                    { name: "Product Page", status: "ok" }, { name: "Price Compare", status: "ok" }, { name: "Review Scrape", status: "run" },
                    { name: "Category Map", status: "ok" }, { name: "Stock Check", status: "run" }, { name: "Image Grab", status: "pending" },
                    { name: "Spec Extract", status: "pending" }, { name: "Brand Match", status: "pending" },
                ]
            },
            {
                name: "Jakarta", progress: 30, total: 50, done: 15, status: "active",
                keywords: [
                    { name: "Marketplace A", status: "ok" }, { name: "Marketplace B", status: "run" }, { name: "Flash Sale", status: "pending" },
                    { name: "Seller Rating", status: "ok" }, { name: "Delivery Zone", status: "pending" },
                ]
            },
            {
                name: "Manila", progress: 0, total: 35, done: 0, status: "pending",
                keywords: [
                    { name: "Local Shop", status: "pending" }, { name: "COD Listing", status: "pending" }, { name: "Promo Code", status: "pending" },
                ]
            },
        ]
    },
    {
        id: "t12", name: "Inventory Sync", status: "SCHEDULED",
        cities: [
            {
                name: "Shenzhen", progress: 0, total: 200, done: 0, status: "pending",
                keywords: [
                    { name: "SKU Match", status: "pending" }, { name: "Stock Level", status: "pending" }, { name: "Price Update", status: "pending" },
                    { name: "Variant Sync", status: "pending" }, { name: "Image Link", status: "pending" }, { name: "Weight Calc", status: "pending" },
                    { name: "Barcode Scan", status: "pending" }, { name: "Category Tag", status: "pending" },
                ]
            },
            {
                name: "Guangzhou", progress: 0, total: 150, done: 0, status: "pending",
                keywords: [
                    { name: "Warehouse A", status: "pending" }, { name: "Warehouse B", status: "pending" }, { name: "Cross-dock", status: "pending" },
                    { name: "Returns Pool", status: "pending" }, { name: "Restock Alert", status: "pending" },
                ]
            },
        ]
    },
];

// ── 设备 → 可用任务 ID 映射（模拟服务端返回）──

export const DEVICE_TASK_MAP: Record<string, string[]> = {
    "*": ["t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9", "t10", "t11", "t12"],
};

// ── Helper: 获取设备的任务列表 ──

export function getTasksForDevice(serial: string): MockTask[] {
    const taskIds = DEVICE_TASK_MAP[serial] ?? DEVICE_TASK_MAP["*"] ?? [];
    return taskIds
        .map(id => MOCK_TASKS.find(t => t.id === id))
        .filter((t): t is MockTask => t !== undefined);
}

// ── Helper: 合并多台设备任务（去重）──

export function mergeDeviceTasks(serials: string[]): MockTask[] {
    const seen = new Set<string>();
    const result: MockTask[] = [];
    for (const serial of serials) {
        for (const task of getTasksForDevice(serial)) {
            if (!seen.has(task.id)) {
                seen.add(task.id);
                result.push(task);
            }
        }
    }
    return result;
}
