-- MusicPlus 授权服务 · Supabase 建表脚本
-- 在 Supabase Dashboard → SQL Editor 执行一次即可。
-- 服务端使用 service_role key 访问（自动绕过 RLS）；anon key 一律拒绝。

-- ── 兑换码（MPR- 发卡链路）──────────────────────────────
create table if not exists redeem_codes (
  code       text primary key,
  serial     text        not null,          -- u64 十进制，同一笔购买共享
  edition    smallint    not null default 1,
  order_no   text,
  created_at bigint      not null            -- unix 秒
);
create index if not exists idx_redeem_codes_order on redeem_codes (order_no);

create table if not exists redeem_bindings (
  code         text    not null references redeem_codes(code) on delete cascade,
  machine_id   char(32) not null,
  license_code text    not null,
  bound_at     bigint  not null,
  primary key (code, machine_id)
);

-- ── 收银台会话（Pancake pull/webhook 幂等核心）────────────
create table if not exists checkout_sessions (
  session_id   text primary key,
  purchase_id  text unique,                  -- 传给 Pancake 的一次性随机 ID
  event_id     text unique,                  -- webhook delivery id 幂等
  machine_id   char(32) not null,
  status       text not null default 'pending',  -- pending | issued
  license_code text,
  paid_at      bigint,                       -- unix 秒
  order_id     text,                         -- Pancake 订单 ID
  created_at   timestamptz not null default now()
);
create index if not exists idx_checkout_machine on checkout_sessions (machine_id);

-- ── 原子兑换 RPC：查上限 → 幂等插入，一个事务内完成 ─────────
-- Vercel 实例无状态，不能用应用层事务；并发兑换同一码时靠此函数串行化。
create or replace function mp_redeem_bind(
  p_code    text,
  p_machine text,
  p_license text,
  p_bound   bigint,
  p_max     int default 2
) returns jsonb
language plpgsql
security definer
set search_path = public
as $$
declare
  v_cnt int;
begin
  -- 兑换码不存在
  if not exists (select 1 from redeem_codes where code = p_code) then
    return jsonb_build_object('ok', false, 'reason', 'NOT_FOUND');
  end if;

  -- 锁行，串行化同一兑换码的并发兑换
  perform pg_advisory_xact_lock(hashtextextended(p_code, 0));

  select count(*) into v_cnt from redeem_bindings where code = p_code;

  -- 新机器且名额已满
  if not exists (
    select 1 from redeem_bindings where code = p_code and machine_id = p_machine
  ) and v_cnt >= p_max then
    return jsonb_build_object('ok', false, 'reason', 'QUOTA_FULL');
  end if;

  insert into redeem_bindings (code, machine_id, license_code, bound_at)
  values (p_code, p_machine, p_license, p_bound)
  on conflict (code, machine_id)
    do update set license_code = excluded.license_code;  -- 同机重兑幂等

  return jsonb_build_object('ok', true);
end;
$$;

-- ── RLS：开启但不建任何 anon 策略 = 公网完全不可访问 ─────────
alter table redeem_codes     enable row level security;
alter table redeem_bindings  enable row level security;
alter table checkout_sessions enable row level security;
