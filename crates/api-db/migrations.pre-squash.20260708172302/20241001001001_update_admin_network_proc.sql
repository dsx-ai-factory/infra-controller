create or replace procedure update_admin_network(
  admin_network uuid,
  new_range inet,
  new_gw inet
)
language plpgsql    
as $$
declare
  v_num_reserved int;
  v_uuid_list uuid[];
  v_uuid uuid;
  v_next_ip inet;
  v_next_hostname varchar(63);
begin
  drop table if exists tmp_network_range_placeholder;
  create table tmp_network_range_placeholder (used_ip inet);

  if not exists (select * From network_segments s join network_prefixes p on p.segment_id = s.id where network_segment_type = 'admin' and s.id = admin_network) then
    raise notice 'Could not find admin network range';
    return;
  end if ;

  if not masklen(new_range) <= (select masklen(p.prefix) From network_segments s join network_prefixes p on p.segment_id = s.id where network_segment_type = 'admin' and s.id = admin_network) then
    raise notice 'New range is smaller than old range, aborting because it cannot be guaranteed to have enough space';
    return;
  end if ;

  if not host(new_range +1) = host(new_gw) then
    raise notice 'new range first IP is not the gateway, exiting';
    return;
  end if ;

  select num_reserved into v_num_reserved From network_segments s join network_prefixes p on p.segment_id = s.id where network_segment_type = 'admin' and s.id = admin_network;

  if v_num_reserved = 0 then
    raise notice 'num_reserved is zero but we need to at least reserve the network and gateway, so making it 2';
    v_num_reserved := 2;
  end if ;

  raise notice 'inserting % rows into placeholder table using new network', v_num_reserved;
  for i in 0..v_num_reserved-1 LOOP
    insert into tmp_network_range_placeholder values (new_range + i);
  end loop ;

  select array_agg(i.id) from machine_interfaces i join machine_interface_addresses a on a.interface_id = i.id and segment_id = admin_network into v_uuid_list;

  foreach v_uuid in array v_uuid_list LOOP
    select host(max(used_ip)+1)::inet, regexp_replace(host(max(used_ip)+1), '\.', '-', 'g') into v_next_ip, v_next_hostname from tmp_network_range_placeholder;
    raise notice 'adding %,  % for %', v_next_ip, v_next_hostname, v_uuid;
    insert into tmp_network_range_placeholder values (v_next_ip);
    update machine_interfaces set hostname = v_next_hostname where id = v_uuid;
    update machine_interface_addresses set address = v_next_ip where interface_id = v_uuid;
  end loop;

  update network_prefixes set gateway = new_gw, prefix = new_range where segment_id = admin_network;

end $$;

