/* ═══════════════════════════════════════
   Mock Data — 统一管理
   ═══════════════════════════════════════ */

// ── Types ──

export type TaskStatus = 'EXECUTING' | 'WAITING' | 'SCHEDULED' | 'SUCCESS' | 'ERROR';

export interface KeywordData {
    name: string;
    status: 'ok' | 'run' | 'pending';
}

export interface CityData {
    name: string;
    progress: number;
    total: number;
    done: number;
    poi: string; // 兴趣点 (Point of Interest)
    status: 'active' | 'done' | 'pending';
    keywords: KeywordData[];
}

export interface MockTask {
    id: string;
    name: string;
    status: TaskStatus;
    assignedDevice?: string;
    cities: CityData[];
}

// ── 任务池（精简为 5 个，覆盖所有状态）──

export const MOCK_TASKS: MockTask[] = [
    {
        id: 't1',
        name: 'Global Lead Gen',
        status: 'EXECUTING',
        assignedDevice: 'R5CT90GXYWJ',
        cities: [
            {
                name: 'Tokyo',
                progress: 44,
                total: 18,
                done: 8,
                poi: 'Shinjuku · Shibuya · Akihabara',
                status: 'active',
                keywords: [
                    { name: 'Tech Startup', status: 'ok' },
                    { name: 'SaaS Company', status: 'ok' },
                    { name: 'Robotics Lab', status: 'run' },
                    { name: 'Design Agency', status: 'pending' },
                    { name: 'Coffee Shop', status: 'ok' },
                    { name: 'Co-working Space', status: 'pending' },
                    { name: 'Boutique Hotel', status: 'pending' },
                    { name: 'Yoga Studio', status: 'pending' },
                    { name: 'Fitness Center', status: 'ok' },
                    { name: 'Art Gallery', status: 'pending' },
                    { name: 'Sushi Bar', status: 'ok' },
                    { name: 'Bakery', status: 'pending' },
                    { name: 'Law Firm', status: 'ok' },
                    { name: 'Dental Clinic', status: 'pending' },
                    { name: 'Museum', status: 'ok' },
                    { name: 'Cinema', status: 'pending' },
                    { name: 'Univ Campus', status: 'ok' },
                    { name: 'Public Library', status: 'run' },
                ],
            },
            {
                name: 'Seoul',
                progress: 100,
                total: 9,
                done: 9,
                poi: 'Gangnam · Hongdae · Myeongdong',
                status: 'done',
                keywords: [
                    { name: 'K-Beauty Store', status: 'ok' },
                    { name: 'PC Bang', status: 'ok' },
                    { name: 'Hanbok Rental', status: 'ok' },
                    { name: 'Fried Chicken', status: 'ok' },
                    { name: 'Language School', status: 'ok' },
                    { name: 'Cafe Chain', status: 'ok' },
                    { name: 'Co-living Space', status: 'ok' },
                    { name: 'Gaming Studio', status: 'ok' },
                    { name: 'Streetwear Shop', status: 'ok' },
                ],
            },
            {
                name: 'Osaka',
                progress: 0,
                total: 6,
                done: 0,
                poi: 'Dotonbori · Namba · Umeda',
                status: 'pending',
                keywords: [
                    { name: 'Ramen Shop', status: 'pending' },
                    { name: 'Theme Park', status: 'pending' },
                    { name: 'Electronics Mall', status: 'pending' },
                    { name: 'Capsule Hotel', status: 'pending' },
                    { name: 'Vintage Store', status: 'pending' },
                    { name: 'Harbor Cruise', status: 'pending' },
                ],
            },
            {
                name: 'Taipei',
                progress: 0,
                total: 8,
                done: 0,
                poi: 'Ximending · Taipei 101 · Jiufen',
                status: 'pending',
                keywords: [
                    { name: 'Night Market', status: 'pending' },
                    { name: 'Tea Shop', status: 'pending' },
                    { name: 'Hot Spring', status: 'pending' },
                    { name: 'Bubble Tea', status: 'pending' },
                    { name: 'Temple', status: 'pending' },
                    { name: 'Electronics St', status: 'pending' },
                    { name: 'Art District', status: 'pending' },
                    { name: 'Bookstore', status: 'pending' },
                ],
            },
            {
                name: 'Bangkok',
                progress: 0,
                total: 7,
                done: 0,
                poi: 'Sukhumvit · Chatuchak · Khao San',
                status: 'pending',
                keywords: [
                    { name: 'Street Food', status: 'pending' },
                    { name: 'Temple Tour', status: 'pending' },
                    { name: 'Floating Market', status: 'pending' },
                    { name: 'Silk Shop', status: 'pending' },
                    { name: 'Rooftop Bar', status: 'pending' },
                    { name: 'Spa Resort', status: 'pending' },
                    { name: 'Boxing Gym', status: 'pending' },
                ],
            },
            {
                name: 'Singapore',
                progress: 0,
                total: 5,
                done: 0,
                poi: 'Marina Bay · Orchard · Sentosa',
                status: 'pending',
                keywords: [
                    { name: 'Hawker Center', status: 'pending' },
                    { name: 'Tech Park', status: 'pending' },
                    { name: 'Marina Bay', status: 'pending' },
                    { name: 'Botanic Garden', status: 'pending' },
                    { name: 'Shopping Mall', status: 'pending' },
                ],
            },
        ],
    },
    {
        id: 't2',
        name: 'Market Audit Pro',
        status: 'EXECUTING',
        assignedDevice: 'PX6P-772A-09',
        cities: [
            {
                name: 'New York',
                progress: 72,
                total: 9,
                done: 6,
                poi: 'Manhattan · Brooklyn · SoHo',
                status: 'active',
                keywords: [
                    { name: 'Fintech Hub', status: 'ok' },
                    { name: 'Ad Agency', status: 'ok' },
                    { name: 'Media Company', status: 'run' },
                    { name: 'Law Practice', status: 'ok' },
                    { name: 'Fashion Brand', status: 'run' },
                    { name: 'Gallery Space', status: 'pending' },
                    { name: 'Coworking HQ', status: 'ok' },
                    { name: 'Restaurant Chain', status: 'ok' },
                    { name: 'Health Clinic', status: 'pending' },
                ],
            },
            {
                name: 'London',
                progress: 35,
                total: 6,
                done: 2,
                poi: 'Westminster · Camden · Soho',
                status: 'active',
                keywords: [
                    { name: 'Insurance Co', status: 'ok' },
                    { name: 'Pub & Bar', status: 'run' },
                    { name: 'Theatre Group', status: 'pending' },
                    { name: 'EdTech Firm', status: 'ok' },
                    { name: 'Royal Tours', status: 'pending' },
                    { name: 'Banking HQ', status: 'pending' },
                ],
            },
        ],
    },
    {
        id: 't3',
        name: 'Data Scraper AI',
        status: 'SCHEDULED',
        cities: [
            {
                name: 'Shanghai',
                progress: 0,
                total: 9,
                done: 0,
                poi: "Pudong · The Bund · Jing'an",
                status: 'pending',
                keywords: [
                    { name: 'Electronics', status: 'pending' },
                    { name: 'Fashion Mall', status: 'pending' },
                    { name: 'Tea House', status: 'pending' },
                    { name: 'Logistics Co', status: 'pending' },
                    { name: 'B2B Platform', status: 'pending' },
                    { name: 'Street Market', status: 'pending' },
                    { name: 'IoT Factory', status: 'pending' },
                    { name: 'Jewelry Store', status: 'pending' },
                    { name: 'Auto Parts', status: 'pending' },
                ],
            },
        ],
    },
    {
        id: 't4',
        name: 'Daily Sync Root',
        status: 'SUCCESS',
        cities: [
            {
                name: 'Local',
                progress: 100,
                total: 3,
                done: 3,
                poi: 'Server Room',
                status: 'done',
                keywords: [
                    { name: 'Config Sync', status: 'ok' },
                    { name: 'DB Backup', status: 'ok' },
                    { name: 'Log Rotate', status: 'ok' },
                ],
            },
        ],
    },
    {
        id: 't5',
        name: 'Social Monitor',
        status: 'ERROR',
        cities: [
            {
                name: 'Global',
                progress: 25,
                total: 5,
                done: 1,
                poi: 'All Regions',
                status: 'active',
                keywords: [
                    { name: 'Twitter Feed', status: 'ok' },
                    { name: 'Instagram API', status: 'run' },
                    { name: 'TikTok Scrape', status: 'pending' },
                    { name: 'Reddit Watch', status: 'pending' },
                    { name: 'YouTube Alert', status: 'pending' },
                ],
            },
        ],
    },
];

// ── 设备 → 可用任务 ID 映射 ──

export const DEVICE_TASK_MAP: Record<string, string[]> = {
    '*': ['t1', 't2', 't3', 't4', 't5'],
};

// ── Helper: 获取设备的任务列表 ──

export function getTasksForDevice(serial: string): MockTask[] {
    const taskIds = DEVICE_TASK_MAP[serial] ?? DEVICE_TASK_MAP['*'] ?? [];
    return taskIds
        .map(id => MOCK_TASKS.find(t => t.id === id))
        .filter((t): t is MockTask => t !== undefined);
}
