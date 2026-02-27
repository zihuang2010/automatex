/* ═══════════════════════════════════════
   Mock Data — 统一管理
   ═══════════════════════════════════════ */

// ── Types ──

export type TaskStatus = 'EXECUTING' | 'WAITING' | 'PAUSED' | 'SCHEDULED' | 'SUCCESS' | 'ERROR';

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

// ── 任务池 ──

export const MOCK_TASKS: MockTask[] = [
    {
        id: 't1',
        name: 'Global Lead Gen',
        status: 'WAITING',
        cities: [
            {
                name: 'Tokyo',
                progress: 0,
                total: 18,
                done: 0,
                poi: 'Shinjuku · Shibuya · Akihabara',
                status: 'pending',
                keywords: [
                    { name: 'Tech Startup', status: 'pending' },
                    { name: 'SaaS Company', status: 'pending' },
                    { name: 'Robotics Lab', status: 'pending' },
                    { name: 'Design Agency', status: 'pending' },
                    { name: 'Coffee Shop', status: 'pending' },
                    { name: 'Co-working Space', status: 'pending' },
                    { name: 'Boutique Hotel', status: 'pending' },
                    { name: 'Yoga Studio', status: 'pending' },
                    { name: 'Fitness Center', status: 'pending' },
                    { name: 'Art Gallery', status: 'pending' },
                    { name: 'Sushi Bar', status: 'pending' },
                    { name: 'Bakery', status: 'pending' },
                    { name: 'Law Firm', status: 'pending' },
                    { name: 'Dental Clinic', status: 'pending' },
                    { name: 'Museum', status: 'pending' },
                    { name: 'Cinema', status: 'pending' },
                    { name: 'Univ Campus', status: 'pending' },
                    { name: 'Public Library', status: 'pending' },
                ],
            },
            {
                name: 'Seoul',
                progress: 0,
                total: 9,
                done: 0,
                poi: 'Gangnam · Hongdae · Myeongdong',
                status: 'pending',
                keywords: [
                    { name: 'K-Beauty Store', status: 'pending' },
                    { name: 'PC Bang', status: 'pending' },
                    { name: 'Hanbok Rental', status: 'pending' },
                    { name: 'Fried Chicken', status: 'pending' },
                    { name: 'Language School', status: 'pending' },
                    { name: 'Cafe Chain', status: 'pending' },
                    { name: 'Co-living Space', status: 'pending' },
                    { name: 'Gaming Studio', status: 'pending' },
                    { name: 'Streetwear Shop', status: 'pending' },
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
        status: 'WAITING',
        cities: [
            {
                name: 'New York',
                progress: 0,
                total: 9,
                done: 0,
                poi: 'Manhattan · Brooklyn · SoHo',
                status: 'pending',
                keywords: [
                    { name: 'Fintech Hub', status: 'pending' },
                    { name: 'Ad Agency', status: 'pending' },
                    { name: 'Media Company', status: 'pending' },
                    { name: 'Law Practice', status: 'pending' },
                    { name: 'Fashion Brand', status: 'pending' },
                    { name: 'Gallery Space', status: 'pending' },
                    { name: 'Coworking HQ', status: 'pending' },
                    { name: 'Restaurant Chain', status: 'pending' },
                    { name: 'Health Clinic', status: 'pending' },
                ],
            },
            {
                name: 'London',
                progress: 0,
                total: 6,
                done: 0,
                poi: 'Westminster · Camden · Soho',
                status: 'pending',
                keywords: [
                    { name: 'Insurance Co', status: 'pending' },
                    { name: 'Pub & Bar', status: 'pending' },
                    { name: 'Theatre Group', status: 'pending' },
                    { name: 'EdTech Firm', status: 'pending' },
                    { name: 'Royal Tours', status: 'pending' },
                    { name: 'Banking HQ', status: 'pending' },
                ],
            },
        ],
    },
    {
        id: 't3',
        name: 'Data Scraper AI',
        status: 'WAITING',
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
        status: 'WAITING',
        cities: [
            {
                name: 'Local',
                progress: 0,
                total: 3,
                done: 0,
                poi: 'Server Room',
                status: 'pending',
                keywords: [
                    { name: 'Config Sync', status: 'pending' },
                    { name: 'DB Backup', status: 'pending' },
                    { name: 'Log Rotate', status: 'pending' },
                ],
            },
        ],
    },
    {
        id: 't5',
        name: 'Social Monitor',
        status: 'WAITING',
        cities: [
            {
                name: 'Global',
                progress: 0,
                total: 5,
                done: 0,
                poi: 'All Regions',
                status: 'pending',
                keywords: [
                    { name: 'Twitter Feed', status: 'pending' },
                    { name: 'Instagram API', status: 'pending' },
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

// ── 任务状态操作函数 ──

/** 设备属性接口（仅用于选择策略） */
interface DevicePropsForSelection {
    battery_level: number;
}

/**
 * 自动选择电量最高的就绪设备
 */
function pickBestDevice(
    readySerials: string[],
    propsMap: Map<string, DevicePropsForSelection>,
): string | null {
    if (!readySerials.length) return null;

    // 按电量降序排序，取第一台
    const sorted = [...readySerials].sort((a, b) => {
        const ba = propsMap.get(a)?.battery_level ?? 0;
        const bb = propsMap.get(b)?.battery_level ?? 0;
        return bb - ba;
    });
    return sorted[0];
}

/**
 * 获取当前所有已被分配设备的 serial 集合
 */
export function getAssignedDeviceSerials(): Set<string> {
    return new Set(
        MOCK_TASKS.filter(
            t => t.assignedDevice && (t.status === 'EXECUTING' || t.status === 'PAUSED'),
        ).map(t => t.assignedDevice!),
    );
}

/**
 * 释放已离线设备上的任务：EXECUTING/PAUSED → WAITING
 * @param onlineSerials 当前在线设备的 serial 集合
 * @returns 被释放的任务数量
 */
export function releaseTasksForOfflineDevices(onlineSerials: Set<string>): number {
    let released = 0;
    for (const task of MOCK_TASKS) {
        if (
            task.assignedDevice &&
            (task.status === 'EXECUTING' || task.status === 'PAUSED') &&
            !onlineSerials.has(task.assignedDevice)
        ) {
            task.status = 'ERROR';
            // 保留 assignedDevice，用户可选择「继续」恢复原设备
            released++;
        }
    }
    return released;
}

/**
 * 启动任务：WAITING → EXECUTING，自动分配电量最高的就绪设备
 * @returns 分配结果 { ok, serial?, error? }
 */
export function startTask(
    taskId: string,
    readySerials: string[],
    propsMap: Map<string, DevicePropsForSelection>,
): { ok: boolean; serial?: string; error?: string } {
    const task = MOCK_TASKS.find(t => t.id === taskId);
    if (!task) return { ok: false, error: '任务不存在' };
    if (task.status !== 'WAITING') return { ok: false, error: '任务状态不正确' };

    // 排除已被其他任务占用的设备
    const assigned = getAssignedDeviceSerials();
    const available = readySerials.filter(s => !assigned.has(s));

    const serial = pickBestDevice(available, propsMap);
    if (!serial) return { ok: false, error: '无可用的就绪设备' };

    task.assignedDevice = serial;
    task.status = 'EXECUTING';
    // 将第一个 pending 城市激活
    const firstPending = task.cities.find(c => c.status === 'pending');
    if (firstPending) firstPending.status = 'active';

    return { ok: true, serial };
}

/**
 * 暂停任务：EXECUTING → PAUSED，保留设备绑定和进度
 */
export function pauseTask(taskId: string): { ok: boolean; error?: string } {
    const task = MOCK_TASKS.find(t => t.id === taskId);
    if (!task) return { ok: false, error: '任务不存在' };
    if (task.status !== 'EXECUTING') return { ok: false, error: '只能暂停执行中的任务' };

    task.status = 'PAUSED';
    return { ok: true };
}

/**
 * 继续任务：PAUSED/ERROR → EXECUTING，保留进度
 * PAUSED 时使用原绑定设备；ERROR 时如果原设备不可用则重新分配
 */
export function resumeTask(
    taskId: string,
    readySerials?: string[],
    propsMap?: Map<string, DevicePropsForSelection>,
): { ok: boolean; serial?: string; error?: string } {
    const task = MOCK_TASKS.find(t => t.id === taskId);
    if (!task) return { ok: false, error: '任务不存在' };
    if (task.status !== 'PAUSED' && task.status !== 'ERROR')
        return { ok: false, error: '只能继续暂停或出错的任务' };

    // PAUSED 时直接恢复原设备
    if (task.status === 'PAUSED' && task.assignedDevice) {
        task.status = 'EXECUTING';
        return { ok: true, serial: task.assignedDevice };
    }

    // ERROR 时可能需要重新分配设备
    if (task.assignedDevice) {
        // 尝试继续使用原设备
        task.status = 'EXECUTING';
        return { ok: true, serial: task.assignedDevice };
    }

    // 原设备不可用，需要重新分配
    if (!readySerials || !propsMap) return { ok: false, error: '无可用设备信息' };
    const assigned = getAssignedDeviceSerials();
    const available = readySerials.filter(s => !assigned.has(s));
    const serial = pickBestDevice(available, propsMap);
    if (!serial) return { ok: false, error: '无可用的就绪设备' };

    task.assignedDevice = serial;
    task.status = 'EXECUTING';
    return { ok: true, serial };
}

/**
 * 停止任务：EXECUTING/PAUSED → WAITING，释放设备，保留进度
 */
export function stopTask(taskId: string): { ok: boolean; error?: string } {
    const task = MOCK_TASKS.find(t => t.id === taskId);
    if (!task) return { ok: false, error: '任务不存在' };
    if (task.status !== 'EXECUTING' && task.status !== 'PAUSED')
        return { ok: false, error: '只能停止执行中或暂停的任务' };

    task.status = 'WAITING';
    task.assignedDevice = undefined;
    return { ok: true };
}

/**
 * 重跑任务：ERROR/SUCCESS → EXECUTING，清零所有进度，重新分配设备
 */
export function retryTask(
    taskId: string,
    readySerials: string[],
    propsMap: Map<string, DevicePropsForSelection>,
): { ok: boolean; serial?: string; error?: string } {
    const task = MOCK_TASKS.find(t => t.id === taskId);
    if (!task) return { ok: false, error: '任务不存在' };
    if (task.status !== 'ERROR' && task.status !== 'SUCCESS')
        return { ok: false, error: '只能重跑出错或已完成的任务' };

    // 清零所有进度
    task.cities.forEach(c => {
        c.progress = 0;
        c.done = 0;
        c.status = 'pending';
        c.keywords.forEach(k => {
            k.status = 'pending';
        });
    });

    // 重新分配设备
    task.assignedDevice = undefined;
    const assigned = getAssignedDeviceSerials();
    const available = readySerials.filter(s => !assigned.has(s));
    const serial = pickBestDevice(available, propsMap);
    if (!serial) {
        task.status = 'WAITING';
        return { ok: false, error: '无可用的就绪设备，任务已重置为等待' };
    }

    task.assignedDevice = serial;
    task.status = 'EXECUTING';
    // 激活第一个城市
    const firstCity = task.cities[0];
    if (firstCity) firstCity.status = 'active';

    return { ok: true, serial };
}

// ═══════════════════════════════════════
//  任务模拟执行引擎
//  每个 EXECUTING 任务一个 interval，每 3 秒推进一个关键词
// ═══════════════════════════════════════

const taskTimers = new Map<string, ReturnType<typeof setInterval>>();

/** UI 更新回调（由 main.ts 设置） */
let tickCallback: (() => void) | null = null;
export function setTaskTickCallback(cb: () => void) {
    tickCallback = cb;
}

/** 启动任务执行（每 3 秒处理一个关键词） */
export function startTaskExecution(taskId: string) {
    if (taskTimers.has(taskId)) return; // 已在运行

    const timer = setInterval(() => {
        const task = MOCK_TASKS.find(t => t.id === taskId);
        if (!task || task.status !== 'EXECUTING') {
            clearInterval(timer);
            taskTimers.delete(taskId);
            return;
        }

        // 找到当前 active 城市
        let activeCity = task.cities.find(c => c.status === 'active');
        if (!activeCity) {
            // 找第一个 pending 城市
            activeCity = task.cities.find(c => c.status === 'pending');
            if (activeCity) activeCity.status = 'active';
        }
        if (!activeCity) {
            // 所有城市都完成了
            task.status = 'SUCCESS';
            clearInterval(timer);
            taskTimers.delete(taskId);
            tickCallback?.();
            return;
        }

        // 找到当前城市中下一个 pending 关键词
        const nextKw = activeCity.keywords.find(k => k.status === 'pending');
        if (nextKw) {
            // 先把之前的 run 状态改为 ok
            activeCity.keywords.forEach(k => {
                if (k.status === 'run') k.status = 'ok';
            });
            nextKw.status = 'run';
            activeCity.done++;
            activeCity.progress = Math.round((activeCity.done / activeCity.total) * 100);
        } else {
            // 当前城市所有关键词都已完成
            activeCity.keywords.forEach(k => {
                if (k.status === 'run') k.status = 'ok';
            });
            activeCity.status = 'done';
            activeCity.progress = 100;

            // 激活下一个 pending 城市，若无则直接完成任务
            const nextCity = task.cities.find(c => c.status === 'pending');
            if (nextCity) {
                nextCity.status = 'active';
            } else {
                // 所有城市都完成了 → 立即结束
                task.status = 'SUCCESS';
                clearInterval(timer);
                taskTimers.delete(taskId);
            }
        }

        tickCallback?.();
    }, 10000); // 真实环境约 10s 一个关键词

    taskTimers.set(taskId, timer);
}

/** 停止任务执行定时器 */
export function stopTaskExecution(taskId: string) {
    const timer = taskTimers.get(taskId);
    if (timer) {
        clearInterval(timer);
        taskTimers.delete(taskId);
    }
}
