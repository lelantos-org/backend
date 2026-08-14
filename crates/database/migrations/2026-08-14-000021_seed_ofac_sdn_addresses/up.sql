-- Point-in-time seed of the OFAC SDN digital-currency address list into
-- `screened_addresses`, for the `evm` address family.
--
-- Provenance: github.com/0xB10C/ofac-sanctioned-digital-currency-addresses
--   branch  `lists`
--   commit  279f6ba0d282dee13f3cd02ec59556e5403acb54 (2026-08-08)
--   files   sanctioned_addresses_{ETH,ARB,BSC,ETC}.txt
-- Those four files are EVM-family and overlap: 103 lines, 100 distinct
-- addresses. Every entry was checked against `0x[0-9a-fA-F]{40}` and
-- lowercased before being written here.
--
-- Addresses are stored lowercased because that is the normalized form
-- `risk_webserver::domain::address::normalize` produces for `chain = 'evm'`,
-- and lookups are exact `=` matches. A checksummed spelling here would be a
-- silent false negative on a sanctioned address.
--
-- OPERATIONAL NOTE: this is a SNAPSHOT, not a subscription. OFAC amends the
-- SDN list continuously; these rows will drift out of date and nothing in the
-- workspace refreshes them. Re-running this migration does not update
-- anything either — `ON CONFLICT DO NOTHING` makes it idempotent, so a
-- changed risk or reason for an existing address is left alone. Keeping the
-- list current needs an importer, which does not exist yet.
--
-- `source = 'ofac_sdn'` is the provenance tag a later importer should reuse,
-- so a refresh updates these rows rather than adding a parallel set.

INSERT INTO screened_addresses (chain, address, risk, source, reason) VALUES
    ('evm', '0x0330070fd38ec3bb94f58fa55d40368271e9e54a', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x038989cbb1710c72b9920dc4fa529158f463e72c', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x04dba1194ee10112fe6c3207c0687def0e78bacf', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x08723392ed15743cc38513c4925f5e6be5c17243', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x08b2efdcdb8822efe5ad0eae55517cf5dc544251', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x0931ca4d13bb4ba75d9b7132ab690265d749a5e7', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x098b716b8aaf21512996dc57eb0615e2383e2f96', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x0ee5067b06776a89ccc7dc8ee369984ad7db5e06', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x12de548f79a50d2bd05481c8515c1ef5183666a9', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x14779cec0b117d5194c750c55ea1f42086631964', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x1967d8af5bd86a497fb3dd7899a020e47560daaf', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x1999ef52700c34de7ec2b68a28aafb37db0c5ade', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x19aa5fe80d33a56d56c78e82ea5e50e5d80b4dff', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x19f8f2b0915daa12a3f5c9cf01df9e24d53794f7', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x1d19b52b54e7ef5ea1a4b40b616165e798eac9f8', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x1da5821544e25c636c1417ba96ade4cf6d2f9b5a', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x21b8d56bda776bbe68655a16895afd96f5534fed', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x2711d73d559f62f4f855ee21f38378f528e07985', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x2c7dcd774b33e10367f7d6385479e04f97d179dc', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x2f389ce8bd8ff92de3402ffce4691d17fc4f6535', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x308ed4b7b49797e1a98d3818bff6fe5385410370', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x32da24ca413f3e7b53145d4737e172c3bdf81e3e', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x35fb6f6db4fb05e6a4ce86f2c93691425626d4b1', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x39d908dac893cbcb53cc86e0ecc369aa4def1a29', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x3ad9db589d201a710ed237c829c7860ba86510fc', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x3cbded43efdaf0fc77b9c55f6fc9988fcc9b757d', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x3cffd56b47b7b41c56258d9c7731abadc360e073', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x3e37627deaa754090fbfbb8bd226c1ce66d255e9', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x43fa21d92141ba9db43052492e0deee5aa5f0a93', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x48549a34ae37b12f6a30566245176994e17c6b4a', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x4f428c11dc82388fa5136d636e613ad923eb700b', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x4f47bc496083c727c5fbe3ce9cdf2b0f6496270c', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x502371699497d08d5339c870851898d6d72521dd', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x530a64c0ce595026a4a556b703644228179e2d57', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x532b77b33a040587e9fd1800088225f99b8b0e8a', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x53b6936513e738f44fb50d2b9476730c0ab3bfc1', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x5512d943ed1f7c8a43f3435c85f7ab68b30121b0', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x57ec89a0c056163a0314e413320f9b3abe761259', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x5a14e72060c11313e38738009254a90968f58f51', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x5a7a51bfb49f190e5a6060a5bc6052ac14a3b59f', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x5d5b5dafecbf31bdb08bfd3edad4f2694372d0ef', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x5f48c2a71b2cc96e3f0ccae4e39318ff0dc375b2', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x67d40ee1a85bf4a4bb7ffae16de985e8427b6b45', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x6b69e2a7545c166417a80c61a77562052bffa9c5', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x6be0ae71e6c41f2f9d0d1a3b8d0f75e6f6a0b46e', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x6f1ca141a28907f78ebaa64fb83a9088b02a8352', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x72a5843cc08275c8171e582972aa4fda8c397b2a', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x747afb5c7a7fc34b547cd0fdebf9b91759c5a52b', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x76ea76ca4eb727f18956ab93445a94c5280412b9', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x797d7ae72ebddcdea2a346c1834e04d1f8df102b', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x7ced75026204ac29c34bea98905d4c949f27361e', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x7db418b5d567a4e0e8c59ad71be1fce48f3e6107', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x7f19720a857f834887fc9a7bc0a0fbe7fc7f8102', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x7f367cc41522ce07553e823bf3be79a889debe1b', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x7ff9cfad3877f21d41da833e2f775db0569ee3d9', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x83e5bc4ffa856bb84bb88581f5dd62a433a25e0d', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x8576acc5c05d6ce88f4e49bf65bdf0c62f91353c', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x8d79c73daae8630c88de372ba8f57592fa987607', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x8dce2aac0de82bdcaf6b4373b79f94331b8e4995', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x901bb9583b24d97e995513c6778dc6888ab6870e', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x931546d9e66836abf687d2bc64b30407bac8c568', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x95584c303fcd48af5c6b9873015f2ad0ca84eae3', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x961c5be54a2ffc17cf4cb021d863c42dacd47fc1', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x97b1043abd9e6fc31681635166d430a458d14f9c', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x983a81ca6fb1e441266d2fbcb7d8e530ac2e05a2', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x9be599d7867f5e1a2d7ec6db9710df2b98a15573', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x9c2bc757b66f24d60f016b6237f8cdd414a879fa', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0x9f4cda013e354b8fc285bf4b9a60460cee7f7ea9', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xa0e1c89ef1a489c9c7de96311ed5ce5d32c20e4b', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xa7e5d5a720f06526557c513402f2e6b5fa20b008', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xac4cc4b68ea24bbfaac8fd127b67ed445accce22', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xb338962b92cd818d6aef0a32a9ecd01212a71f33', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xb637f84b66876ebf609c2a4208905f9ddac9d075', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xb6f5ec1a0a9cd1526536d3f0426c429529471f40', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xbb69e01921b17cd22080968bcc96ba6115da6062', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xc103b7dc095c904b92081eef0c1640081ec01c10', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xc2a3829f459b3edd87791c74cd45402ba0a20be3', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xc455f7fd3e0e12afd51fba5c106909934d8a0e4a', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xcb74874f1e06fcf80a306e06e5379a44b488ba2d', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xd04e33461fea8302c5e1e13895b60cee8aefda7f', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xd0975b32cea532eadddfc9c60481976e39db3472', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xd5ed34b52ac4ab84d8fa8a231a3218bbf01ed510', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xd8500c631dc32fa18645b7436344a99e4825e10e', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xd882cfc20f52f2599d84b8e8d58c7fb62cfe344b', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xdb2720ebad55399117ddb4c4a4afd9a4ccada8fe', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xdcbeffbecce100cce9e4b153c4e15cb885643193', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xe05f529f5284d75624eba386cb716928c3b54a2a', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xe1d865c3d669dcc8c57c8d023140cb204e672ee4', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xe1e4c5e5ed8f03ae61b581e2def126025f2b9401', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xe3d35f68383732649669aa990832e017340dbca5', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xe7aa314c77f4233c18c6cc84384a9247c0cf367b', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xe950dc316b836e4eefb8308bf32bf7c72a1358ff', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xed6e0a7e4ac94d976eebfb82ccf777a3c6bad921', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xefe301d259f525ca1ba74a7977b80d5b060b3cca', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xf2235d55b2950a0b1317469d72d07ae65b2e27cb', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xf3701f445b6bdafedbca97d1e477357839e4120d', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xf4377eda661e04b6dda78969796ed31658d602d4', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xf7b31119c2682c88d88d455dbb9d5932c65cf1be', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xfb3eff152ea55d1bfa04dbdd509a80fd7b72cdeb', 'banned', 'ofac_sdn', 'OFAC SDN'),
    ('evm', '0xfda1ec4a6178d4916b001a065422d31ebe5f62ff', 'banned', 'ofac_sdn', 'OFAC SDN')
ON CONFLICT (chain, address, source) DO NOTHING;
